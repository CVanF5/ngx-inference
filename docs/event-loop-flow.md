# Event Loop Flow Diagram

This diagram illustrates the NGINX event loop flow when both BBR (Body-Based Routing) and EPP (Endpoint Picker Processor) are enabled.

```mermaid
sequenceDiagram
    participant Client
    participant NGINX as NGINX Event Loop
    participant Handler as inference_access_handler
    participant BBR as BbrProcessor::process_request
    participant BodyReader as ngx_http_read_client_request_body
    participant Callback as bbr_body_read_handler
    participant EPP as EppProcessor::process_request
    participant gRPC as gRPC Client
    participant ExtProc as External Processor
    participant Upstream

    Note over Client,NGINX: Initial Request
    Client->>NGINX: HTTP Request with JSON body
    NGINX->>Handler: Run ACCESS phase handler
    
    Note over Handler,BBR: First Handler Invocation - BBR Processing
    Handler->>BBR: process_request(request, conf)
    BBR->>BBR: Check if bbr_enable == true
    BBR->>BBR: get_header_in("X-Gateway-Model-Name")
    Note over BBR: Header not present (first time)
    BBR->>BBR: start_body_reading(request, conf)
    BBR->>BodyReader: ngx_http_read_client_request_body(r, callback)
    BodyReader-->>BBR: Returns NGX_AGAIN or NGX_OK
    BBR-->>Handler: Returns NGX_DONE
    Handler-->>NGINX: Returns NGX_DONE (pause phases)
    
    Note over NGINX,Callback: Async Body Reading (Event Loop Continues!)
    NGINX->>NGINX: Event loop continues processing OTHER requests
    NGINX->>NGINX: Handle other connections, timers, events
    Note over NGINX: This request's phase processing is paused,<br/>but NGINX serves thousands of other requests
    NGINX->>NGINX: Body chunks arrive from client (async I/O)
    
    Note over Callback: Body Read Complete - Callback Execution
    NGINX->>Callback: bbr_body_read_handler(r)
    Callback->>Callback: Validate request pointer
    Callback->>Callback: Check request_body structure
    Callback->>Callback: get_header_in("X-Gateway-Model-Name")
    Note over Callback: Check header again (defensive)
    Callback->>Callback: Clear post_handler to prevent re-execution
    Callback->>Callback: read_request_body(r, conf)
    Note over Callback: Read from memory/file buffers
    Callback->>Callback: extract_model_from_body(body)
    Callback->>Callback: add_header_in("X-Gateway-Model-Name", model)
    Note over Callback: Header now set!
    Callback->>NGINX: ngx_http_core_run_phases(r)
    Note over NGINX: Resume phase processing
    
    Note over Handler,EPP: Second Handler Invocation - EPP Processing
    NGINX->>Handler: Run ACCESS phase handler (again)
    Handler->>BBR: process_request(request, conf)
    BBR->>BBR: Check if bbr_enable == true
    BBR->>BBR: get_header_in("X-Gateway-Model-Name")
    Note over BBR: Header IS present (set by callback)
    BBR-->>Handler: Returns NGX_DECLINED (skip BBR)
    
    Handler->>EPP: process_request(request, conf)
    EPP->>EPP: Check if epp_enable == true
    EPP->>EPP: pick_upstream(request, conf)
    EPP->>EPP: get_header_in("X-Inference-Upstream")
    Note over EPP: Header not present (first time)
    EPP->>EPP: Collect request headers
    EPP->>gRPC: epp_headers_blocking(endpoint, headers)
    gRPC->>ExtProc: gRPC Request (headers only)
    ExtProc-->>gRPC: Response with upstream selection
    gRPC-->>EPP: Returns upstream value
    EPP->>EPP: add_header_in("X-Inference-Upstream", upstream)
    EPP-->>Handler: Returns Ok(())
    
    Handler-->>NGINX: Returns NGX_DECLINED (continue)
    Note over NGINX: Continue to upstream phase
    NGINX->>NGINX: Read $inference_upstream variable
    NGINX->>NGINX: Variable evaluator reads header
    NGINX->>Upstream: proxy_pass to selected upstream
    Upstream-->>Client: Response
```

## Key Functions and Flow

### First Handler Invocation (BBR)
1. **`inference_access_handler`** - ACCESS phase handler entry point
2. **`BbrProcessor::process_request()`** - Check if BBR needed
3. **`BbrProcessor::start_body_reading()`** - Initiate async body read
4. **`ngx_http_read_client_request_body()`** - NGINX FFI to read body
5. Returns **`NGX_DONE`** to pause phase processing

### Async Body Processing (Callback)
6. **`bbr_body_read_handler()`** - Called when body is ready
7. **`read_request_body()`** - Extract body from buffers
8. **`extract_model_from_body()`** - Parse JSON for model name
9. **`add_header_in()`** - Set `X-Gateway-Model-Name` header
10. **`ngx_http_core_run_phases()`** - Resume NGINX phase processing

### Second Handler Invocation (EPP)
11. **`inference_access_handler`** - Same handler called again
12. **`BbrProcessor::process_request()`** - Checks header, returns `NGX_DECLINED`
13. **`EppProcessor::process_request()`** - Check if EPP needed
14. **`EppProcessor::pick_upstream()`** - Contact external processor
15. **`crate::grpc::epp_headers_blocking()`** - Blocking gRPC call
16. **`add_header_in()`** - Set `X-Inference-Upstream` header
17. Returns **`NGX_DECLINED`** to continue normal processing

### Variable Evaluation
18. **`inference_upstream_var_get()`** - Evaluates `$inference_upstream` variable
19. Reads from `X-Inference-Upstream` header set by EPP

## Important Notes

- **Event loop is NOT paused**: When BBR returns `NGX_DONE`, only THIS request's phase processing pauses. The NGINX event loop continues running, serving other requests and handling other events. This is why NGINX can handle thousands of concurrent connections efficiently.

- **BBR is async**: Returns `NGX_DONE` to yield control back to NGINX event loop. The request body is read asynchronously using non-blocking I/O. NGINX can process other requests while waiting for body chunks to arrive.

- **EPP is sync/blocking**: Makes a blocking gRPC call but completes within the same handler invocation. This means EPP will block the worker process for this request until the gRPC call completes (typically ~200ms or configured timeout).

- **Request-specific pause**: Only the specific request waiting for body reading has its phase processing paused. Other requests continue through their phases normally.

- **Handler runs twice**: The same `inference_access_handler` is invoked twice for the same request - once before body reading (BBR starts async read), and once after (BBR self-skips, EPP executes).

- **Phase resumption**: `ngx_http_core_run_phases()` is the critical function that resumes phase processing for the specific request after the async body read completes.

- **Defensive checks**: Both BBR callback and process_request check for header presence to prevent duplicate processing if the handler is somehow invoked again.
