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
    BBR->>BBR: get_header_in(conf.bbr_header_name)
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
    Callback->>Callback: get_header_in(conf.bbr_header_name)
    Note over Callback: Check header again (defensive)
    Callback->>Callback: Clear post_handler to prevent re-execution
    Callback->>Callback: read_request_body(r, conf)
    Note over Callback: Read from memory/file buffers
    Callback->>Callback: extract_model_from_body(body)
    alt Model found in body
        Callback->>Callback: add_header_in(conf.bbr_header_name, model)
    else No model found
        Callback->>Callback: add_header_in(conf.bbr_header_name, conf.bbr_default_model)
        Note over Callback: Use configured default model
    end
    Note over Callback: Header now set!
    Callback->>NGINX: ngx_http_core_run_phases(r)
    Note over NGINX: Resume phase processing
    
    Note over Handler,EPP: Second Handler Invocation - EPP Processing
    NGINX->>Handler: Run ACCESS phase handler (again)
    Handler->>BBR: process_request(request, conf)
    BBR->>BBR: Check if bbr_enable == true
    BBR->>BBR: get_header_in(conf.bbr_header_name)
    Note over BBR: Header IS present (set by callback)
    BBR-->>Handler: Returns NGX_DECLINED (skip BBR)
    
    Handler->>EPP: process_request(request, conf)
    EPP->>EPP: Check if epp_enable == true
    EPP->>EPP: pick_upstream_blocking(request, conf)
    EPP->>EPP: get_header_in(conf.epp_header_name)
    Note over EPP: Header not present (first time)
    EPP->>EPP: Collect request headers
    EPP->>gRPC: epp_headers_blocking(endpoint, headers)
    Note over gRPC: Blocking call (uses async internally with block_on)
    alt gRPC Success
        gRPC->>ExtProc: gRPC Request (headers only)
        ExtProc-->>gRPC: Response with upstream selection
        gRPC-->>EPP: Returns upstream value
        EPP->>EPP: add_header_in(conf.epp_header_name, upstream)
        EPP-->>Handler: Returns NGX_DECLINED
    else gRPC Failure & epp_failure_mode_allow=true
        gRPC-->>EPP: Returns error
        Note over EPP: Check if default_upstream configured
        EPP->>EPP: add_header_in(conf.epp_header_name, conf.default_upstream)
        EPP-->>Handler: Returns NGX_DECLINED (fail-open)
    else gRPC Failure & epp_failure_mode_allow=false
        gRPC-->>EPP: Returns error
        EPP-->>Handler: Returns NGX_ERROR (fail-closed)
        Handler-->>NGINX: Returns HTTP 500
        NGINX-->>Client: HTTP 500 Internal Server Error
        Note over Client: Request terminated due to EPP failure
    end
        
    Note over Handler,NGINX: Normal Success Path (EPP returns NGX_DECLINED)
    Handler-->>NGINX: Returns NGX_DECLINED (continue)
    Note over NGINX: Continue to upstream phase
    NGINX->>NGINX: Read $inference_upstream variable
    NGINX->>NGINX: Variable evaluator reads conf.epp_header_name header
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
9. **`add_header_in()`** - Set configurable BBR header (default: `X-Gateway-Model-Name`) with extracted model or default model if none found
10. **`ngx_http_core_run_phases()`** - Resume NGINX phase processing

### Second Handler Invocation (EPP)
11. **`inference_access_handler`** - Same handler called again
12. **`BbrProcessor::process_request()`** - Checks header, returns `NGX_DECLINED`
13. **`EppProcessor::process_request()`** - Check if EPP needed
14. **`EppProcessor::pick_upstream_blocking()`** - Contact external processor (blocking)
15. **`crate::grpc::epp_headers_blocking()`** - Blocking gRPC call (async internally)
16. **Error handling**: On success, sets configurable EPP header and returns `NGX_DECLINED`; on failure with `epp_failure_mode_allow=false`, returns `NGX_ERROR` (causing HTTP 500); on failure with `epp_failure_mode_allow=true`, may set default_upstream and returns `NGX_DECLINED`
17. **Handler response**: Returns **`NGX_DECLINED`** on success/fail-open, or **HTTP 500** on fail-closed EPP errors

### Variable Evaluation
18. **`inference_upstream_var_get()`** - Evaluates `$inference_upstream` variable
19. Reads from configurable EPP header (default: `X-Inference-Upstream`) set by EPP or default upstream fallback

## Important Notes

- **Event loop is NOT paused**: When BBR returns `NGX_DONE`, only THIS request's phase processing pauses. The NGINX event loop continues running, serving other requests and handling other events. This is why NGINX can handle thousands of concurrent connections efficiently.

- **BBR is async**: Returns `NGX_DONE` to yield control back to NGINX event loop. The request body is read asynchronously using non-blocking I/O. NGINX can process other requests while waiting for body chunks to arrive.

- **EPP is blocking**: Uses synchronous gRPC calls via `epp_headers_blocking()`. While the gRPC internals use async operations with `tokio::runtime::block_on()`, the NGINX interface remains blocking and respects the single-threaded event loop model. This is simpler and more reliable than async callbacks.

- **EPP failure modes**: EPP supports two failure modes via `epp_failure_mode_allow` directive:
  - **Fail-closed** (`epp_failure_mode_allow off`): EPP failures return `NGX_ERROR`, causing the main handler to return HTTP 500
  - **Fail-open** (`epp_failure_mode_allow on`): EPP failures return `NGX_DECLINED` and may set `default_upstream` if configured, allowing request processing to continue

- **Request-specific pause**: Only the specific request waiting for body reading has its phase processing paused. Other requests continue through their phases normally.

- **Handler runs twice**: The same `inference_access_handler` is invoked twice for the same request - once before body reading (BBR starts async read), and once after (BBR self-skips, EPP executes).

- **Phase resumption**: `ngx_http_core_run_phases()` is the critical function that resumes phase processing for the specific request after the async body read completes.

- **Defensive checks**: Both BBR callback and process_request check for header presence to prevent duplicate processing if the handler is somehow invoked again.

- **Configurable headers**: Both BBR and EPP header names are configurable via `inference_bbr_header_name` and `inference_epp_header_name` directives, defaulting to `X-Gateway-Model-Name` and `X-Inference-Upstream` respectively.

- **Default model handling**: When BBR cannot extract a model from the request body, it uses the configured `bbr_default_model` value to prevent reprocessing and ensure consistent behavior.

- **EPP error handling**: EPP failures are handled according to `epp_failure_mode_allow` setting. In fail-closed mode, EPP errors terminate the request with HTTP 500. In fail-open mode, EPP errors allow the request to continue, optionally using `default_upstream` if configured.

- **Request termination**: Unlike BBR which always allows requests to continue (possibly with default model), EPP can terminate requests early if `epp_failure_mode_allow=false` and the external processor is unavailable.
