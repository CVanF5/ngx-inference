ngx-inference: NGINX module for Gateway API Inference Extensions (EPP + BBR)
=============================================================================

Overview
--------
This project provides a native NGINX module (built with ngx-rust) that integrates with the Gateway API Inference Extension using Envoy's `ext_proc` protocol over gRPC.

It implements:
- Endpoint Picker Processor (EPP): A headers-only exchange to obtain an upstream endpoint hint from the EPP and expose it via the `$inference_upstream` NGINX variable.
- Body-Based Routing (BBR): A streaming-mode handshake to efficiently send request body chunks to a remote BBR implementation and inject the returned model header back into the original request. The current revision performs the handshake and header extraction; streaming of actual request body chunks is scaffolded and can be enabled in a follow-up.

Reference docs:
- NGF design doc: https://github.com/nginx/nginx-gateway-fabric/blob/main/docs/proposals/gateway-inference-extension.md
- EPP reference implementation: https://github.com/kubernetes-sigs/gateway-api-inference-extension/tree/main/pkg/epp

Current behavior and defaults
-----------------------------
- BBR:
  - Directive `inference_bbr on|off` toggles BBR.
  - Directive `inference_bbr_endpoint` sets the gRPC endpoint for the BBR/ext-proc server (plaintext or `http://host:port`; `https://` not yet supported).
  - Directive `inference_bbr_header_name` sets the model header name to inject (default `X-Gateway-Model-Name`).
  - BBR sends a `HttpHeaders` frame and a ProtocolConfiguration with `request_body_mode=STREAMED`, then reads responses to extract a header mutation for the configured model header name. At present, request body streaming is scaffolded; chunked streaming from the NGINX request will be implemented next.

- EPP:
  - Directive `inference_epp on|off` toggles EPP.
  - Directive `inference_epp_endpoint` sets the gRPC endpoint for the EPP/ext-proc server (plaintext or `http://host:port`).
  - Directive `inference_epp_header_name` sets the upstream header name to read and expose (default `X-Inference-Upstream`).
  - EPP runs a headers-only exchange, reads response header mutations, and sets an incoming request header for the configured upstream header name (default `X-Inference-Upstream`) when present.
  - The `$inference_upstream` NGINX variable reflects the value of the configured upstream header and can be used in `proxy_pass`.

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
- Header names:
  - BBR returns and injects a model header (default `X-Gateway-Model-Name`). You can configure this via `inference_bbr_header_name`.
  - EPP should return an endpoint hint via header mutation. This module reads a configurable upstream header via `inference_epp_header_name` (default `X-Inference-Upstream`) and exposes its value as `$inference_upstream`.

- TLS:
  - Current implementation uses insecure/plaintext gRPC channels. The EPP project notes TLS support is a known issue still under discussion. Once TLS configuration is available, this module can be extended to support secure gRPC channels.

- Body streaming:
  - This revision prepares STREAMED mode in the ext-proc protocol and extracts returned header mutations. Actual chunk streaming from the NGINX request body to the ext-proc server will be implemented next, using NGINX request body APIs to read client request body and forward chunks over the tonic bidirectional stream.

- Request headers to ext-proc:
  - The current implementation sends an empty `HeaderMap` in `HttpHeaders`. Depending on your EPP/BBR expectations, this may be sufficient (e.g., when the ModelName is provided by BBR). In a future update, we will forward selected headers (e.g., content-type, model header if present) and attributes to ext-proc for richer context.

Troubleshooting
---------------
- If EPP/BBR endpoints are unreachable or not listening on gRPC, you may see `BAD_GATEWAY` when failure mode allow is off. Toggle `*_failure_mode_allow on` to fail-open during testing.
- Ensure your EPP implementation is configured to return a header mutation for the upstream endpoint. The module will parse response frames and search for `header_mutation` entries.
- Use `error_log` and debug logging to verify module activation. The access-phase handler logs `ngx-inference: bbr_enable=<..> epp_enable=<..>` per request.

Roadmap
-------
- Align exact header names and semantics to the upstream spec and EPP reference.
- Implement true body chunk streaming from NGINX request to ext-proc for BBR, including configurable maximum body size and back-pressure.
- Pass through relevant request headers/attributes to ext-proc to improve routing context.
- Conformance tests using the Gateway API Inference Extension suite.
- TLS support for gRPC once available in EPP.

License
-------
Apache-2.0 (to align with upstream projects).
