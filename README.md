ngx-inference: NGINX module for Gateway API Inference Extensions (EPP + BBR)
=============================================================================

Overview
--------
This project provides a native NGINX module (built with ngx-rust) that implements the Gateway API Inference Extension using Envoy's `ext_proc` protocol over gRPC.

It implements two standard components:
- **Endpoint Picker Processor (EPP)**: Headers-only exchange following the Gateway API Inference Extension specification to obtain upstream endpoint selection and expose endpoints via the `$inference_upstream` NGINX variable.
- **Body-Based Routing (BBR)**: Body streaming implementation that extracts model names from JSON request bodies and injects model headers, following the reference BBR implementation from the Gateway API Inference Extension project.

Reference docs:
- NGF design doc: https://github.com/nginx/nginx-gateway-fabric/blob/main/docs/proposals/gateway-inference-extension.md
- EPP reference implementation: https://github.com/kubernetes-sigs/gateway-api-inference-extension/tree/main/pkg/epp

Current behavior and defaults
-----------------------------
- BBR:
  - Directive `inference_bbr on|off` enables/disables the standard BBR implementation.
  - Directive `inference_bbr_endpoint` sets the gRPC endpoint for BBR ext-proc server communication (plaintext or `http://host:port`; `https://` not yet supported).
  - Directive `inference_bbr_header_name` configures the model header name to inject (default `X-Gateway-Model-Name`).
  - BBR follows the Gateway API specification: sends `HttpHeaders` with `request_body_mode=STREAMED`, streams complete request body in chunks for JSON model extraction, and returns header mutations with the detected model name from the "model" field.

- EPP:
  - Directive `inference_epp on|off` enables/disables EPP functionality.
  - Directive `inference_epp_endpoint` sets the gRPC endpoint for standard EPP ext-proc server communication.
  - Directive `inference_epp_header_name` configures the upstream header name to read from EPP responses (default `X-Inference-Upstream`).
  - EPP follows the Gateway API Inference Extension specification: performs headers-only exchange, reads header mutations from responses, and sets the upstream header for endpoint selection.
  - The `$inference_upstream` NGINX variable exposes the EPP-selected endpoint and can be used in `proxy_pass` directives.

- Fail-open/closed:
  - `inference_bbr_failure_mode_allow on|off` and `inference_epp_failure_mode_allow on|off` control whether to fail-open when the ext-proc is unavailable or errors. Fail-closed returns `502 Bad Gateway`.

Build
-----
Requirements:
- macOS or Linux with Rust toolchain and protoc (tonic-build uses prost/protobuf).
- NGINX with dynamic module support (OSS or Plus). Using ngx-rust requires building a cdylib module and loading it via `load_module`.

Steps:
1. Build the crate and generated protos:
   - `cargo build --features vendored`

2. Build the cdylib with exported modules (for NGINX `ngx_modules` table):
   - `cargo build --features "vendored,export-modules"`

3. The resulting dynamic library can be found under `target/debug/` or `target/release/` depending on profile. Name will be platform-dependent (e.g. `libngx_inference.dylib` on macOS, `libngx_inference.so` on Linux).

NGINX configuration
-------------------
Example configuration snippet for a location using BBR followed by EPP:
```
# Load the compiled module (path depends on your build output)
# load_module /opt/nginx/modules/libngx_inference.so;

http {
    server {
        listen 8080;

        location /inference {
            # Body-Based Routing: request body streamed to BBR/ext-proc; returned model header added to request
            inference_bbr on;
            inference_bbr_endpoint 127.0.0.1:50051;     # your BBR/ext-proc host:port
            inference_bbr_chunk_size 65536;              # recommended <= 64KiB
            inference_bbr_timeout_ms 200;
            inference_bbr_failure_mode_allow on;         # fail-open preferred per alpha stage
            inference_bbr_header_name X-Gateway-Model-Name;

            # Endpoint Picker Processor: headers-only exchange to get upstream endpoint hint
            inference_epp on;
            inference_epp_endpoint 127.0.0.1:50052;      # your EPP/ext-proc host:port
            inference_epp_header_name X-Inference-Upstream;  # upstream header name (default)
            inference_epp_timeout_ms 200;
            inference_epp_failure_mode_allow off;        # fail-closed

            # Use the upstream value returned by EPP
            proxy_pass http://$inference_upstream;
        }
    }
}
```

Notes and assumptions
---------------------
- **Standards Compliance**:
  - Both EPP and BBR implementations follow the Gateway API Inference Extension specification.
  - EPP is compatible with reference EPP servers for endpoint selection.
  - BBR is compatible with reference BBR servers (kubernetes-sigs/gateway-api-inference-extension/pkg/bbr) for model detection from JSON request bodies.

- Header names:
  - BBR returns and injects a model header (default `X-Gateway-Model-Name`). You can configure this via `inference_bbr_header_name`.
  - EPP should return an endpoint hint via header mutation. This module reads a configurable upstream header via `inference_epp_header_name` (default `X-Inference-Upstream`) and exposes its value as `$inference_upstream`.

- TLS:
  - Current implementation uses insecure/plaintext gRPC channels. The EPP project notes TLS support is a known issue still under discussion. Once TLS configuration is available, this module can be extended to support secure gRPC channels.

- Body streaming:
  - EPP follows the standard Gateway API specification with headers-only mode (no body streaming).
  - BBR implements the standard STREAMED mode per the Gateway API specification for body-based model detection from JSON. Matches the reference implementation in kubernetes-sigs/gateway-api-inference-extension/pkg/bbr.

- Request headers to ext-proc:
  - EPP implementation forwards incoming request headers per the Gateway API specification for endpoint selection context.
  - BBR custom implementation sends complete request body for model detection, providing model-based routing beyond the standard specification.

Troubleshooting
---------------
- If EPP/BBR endpoints are unreachable or not listening on gRPC, you may see `BAD_GATEWAY` when failure mode allow is off. Toggle `*_failure_mode_allow on` to fail-open during testing.
- Ensure your EPP implementation is configured to return a header mutation for the upstream endpoint. The module will parse response frames and search for `header_mutation` entries.
- Use `error_log` and debug logging to verify module activation. The access-phase handler logs `ngx-inference: bbr_enable=<..> epp_enable=<..>` per request.

Roadmap
-------
- Validate EPP and BBR implementations against Gateway API Inference Extension conformance tests.
- Align exact header names and semantics to the upstream specification and reference implementations.
- Add configurable maximum body size and back-pressure handling for BBR.
- TLS support for gRPC once available in the Gateway API specification.
- Connection pooling and caching for improved performance at scale.

License
-------
Apache-2.0 (to align with upstream projects).
