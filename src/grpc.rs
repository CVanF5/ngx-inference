//! gRPC client implementation for Envoy ExternalProcessor (ext-proc) protocol.
//!
//! This module implements EPP (Endpoint Picker Processor) for Gateway API Inference Extension:
//! - Headers-only exchange for upstream endpoint selection
//!
//! The implementation follows the Gateway API Inference Extension specification.

use crate::protos::envoy;
use ngx::{http, ngx_log_debug_http};

use std::sync::OnceLock;

use tonic::transport::Channel;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2) // Minimal thread pool for gRPC operations
            .enable_all()
            .thread_name("ngx-inference-grpc")
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

type ExternalProcessorClient<T> =
    envoy::service::ext_proc::v3::external_processor_client::ExternalProcessorClient<T>;

type ProcessingRequest = envoy::service::ext_proc::v3::ProcessingRequest;
type ProcessingResponse = envoy::service::ext_proc::v3::ProcessingResponse;

type ProtocolConfiguration = envoy::service::ext_proc::v3::ProtocolConfiguration;
type BodySendMode = envoy::extensions::filters::http::ext_proc::v3::processing_mode::BodySendMode;

type HttpHeaders = envoy::service::ext_proc::v3::HttpHeaders;
type HeaderMap = envoy::config::core::v3::HeaderMap;

fn normalize_endpoint(endpoint: &str, use_tls: bool) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else if use_tls {
        format!("https://{}", endpoint)
    } else {
        format!("http://{}", endpoint)
    }
}

fn extract_header_from_mutation(
    request: &http::Request,
    mutation: &envoy::service::ext_proc::v3::HeaderMutation,
    target_key_lower: &str,
) -> Option<String> {
    ngx_log_debug_http!(
        request,
        "ngx-inference: Searching for header '{}' in mutation with {} headers",
        target_key_lower,
        mutation.set_headers.len()
    );

    // Log all available headers for debugging
    for (i, hvo) in mutation.set_headers.iter().enumerate() {
        if let Some(hdr) = &hvo.header {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Header[{}]: key='{}', value='{}', raw_value_len={}",
                i,
                hdr.key,
                hdr.value,
                hdr.raw_value.len()
            );
        }
    }

    for hvo in &mutation.set_headers {
        if let Some(hdr) = &hvo.header {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Comparing '{}' == '{}' (ignore case)",
                hdr.key,
                target_key_lower
            );
            // Keys are lower-cased in HttpHeaders; we compare ASCII-case-insensitively just in case.
            if hdr.key.eq_ignore_ascii_case(target_key_lower) {
                if !hdr.value.is_empty() {
                    let value = hdr.value.clone();
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found matching header with value: '{}'",
                        value
                    );
                    return Some(value);
                }
                if !hdr.raw_value.is_empty() {
                    let value = String::from_utf8_lossy(&hdr.raw_value).to_string();
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found matching header with raw_value: '{}'",
                        value
                    );
                    return Some(value);
                }
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: Found matching header key but no value"
                );
            }
        }
    }

    ngx_log_debug_http!(
        request,
        "ngx-inference: Target header '{}' not found in header mutation",
        target_key_lower
    );
    None
}

fn parse_response_for_header(
    request: &http::Request,
    resp: &ProcessingResponse,
    target_key_lower: &str,
) -> Option<String> {
    use envoy::service::ext_proc::v3::processing_response;

    ngx_log_debug_http!(
        request,
        "ngx-inference: Parsing response for header '{}'",
        target_key_lower
    );

    match &resp.response {
        Some(processing_response::Response::RequestHeaders(hdrs)) => {
            ngx_log_debug_http!(request, "ngx-inference: Processing RequestHeaders response");
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(request, hm, target_key_lower);
                } else {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: No header mutation in RequestHeaders"
                    );
                }
            } else {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: No common response in RequestHeaders"
                );
            }
        }
        Some(processing_response::Response::ResponseHeaders(hdrs)) => {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Processing ResponseHeaders response"
            );
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(request, hm, target_key_lower);
                } else {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: No header mutation in ResponseHeaders"
                    );
                }
            } else {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: No common response in ResponseHeaders"
                );
            }
        }
        Some(processing_response::Response::RequestBody(body)) => {
            ngx_log_debug_http!(request, "ngx-inference: Processing RequestBody response");
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(request, hm, target_key_lower);
                } else {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: No header mutation in RequestBody"
                    );
                }
            } else {
                ngx_log_debug_http!(request, "ngx-inference: No common response in RequestBody");
            }
        }
        Some(processing_response::Response::ResponseBody(body)) => {
            ngx_log_debug_http!(request, "ngx-inference: Processing ResponseBody response");
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(request, hm, target_key_lower);
                } else {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: No header mutation in ResponseBody"
                    );
                }
            } else {
                ngx_log_debug_http!(request, "ngx-inference: No common response in ResponseBody");
            }
        }
        Some(processing_response::Response::RequestTrailers(tr)) => {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Processing RequestTrailers response"
            );
            if let Some(hm) = &tr.header_mutation {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(request, hm, target_key_lower);
            } else {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: No header mutation in RequestTrailers"
                );
            }
        }
        Some(processing_response::Response::ResponseTrailers(tr)) => {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Processing ResponseTrailers response"
            );
            if let Some(hm) = &tr.header_mutation {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(request, hm, target_key_lower);
            } else {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: No header mutation in ResponseTrailers"
                );
            }
        }
        Some(processing_response::Response::ImmediateResponse(ir)) => {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Processing ImmediateResponse (status: {:?})",
                ir.status
            );
            if let Some(hm) = &ir.headers {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(request, hm, target_key_lower);
            } else {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: No header mutation in ImmediateResponse"
                );
            }
        }
        None => {
            ngx_log_debug_http!(request, "ngx-inference: Response has no content (None)");
        }
    }

    ngx_log_debug_http!(
        request,
        "ngx-inference: No matching header found in response"
    );
    None
}

/// EPP: Request headers and body exchange for upstream endpoint selection.
///
/// Returns Ok(Some(value)) if the ext-proc service replies with a header mutation
/// for the specified header name; Ok(None) if not present; Err(...) on transport-level errors.
pub fn epp_headers_blocking(
    request: &http::Request,
    endpoint: &str,
    timeout_ms: u64,
    header_name: &str,
    headers: Vec<(String, String)>,
    use_tls: bool,
    ca_file: Option<&str>,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint, use_tls);

    get_runtime().block_on(async move {
        let channel_builder =
            Channel::from_shared(uri.clone()).map_err(|e| format!("channel error: {e}"))?;

        // Build the channel with appropriate TLS configuration
        let channel = if use_tls {
            // SECURE MODE: Configure TLS with custom CA if provided, otherwise use system roots
            use tonic::transport::ClientTlsConfig;

            // Extract domain from endpoint for TLS verification
            let domain = if let Some(colon_pos) = endpoint.rfind(':') {
                endpoint[..colon_pos].to_string()
            } else {
                endpoint.to_string()
            };

            ngx_log_debug_http!(request, "ngx-inference: TLS Configuration:");
            ngx_log_debug_http!(request, "ngx-inference:   - Endpoint: {}", endpoint);
            ngx_log_debug_http!(request, "ngx-inference:   - URI: {}", uri);
            ngx_log_debug_http!(request, "ngx-inference:   - Extracted domain: '{}'", domain);
            ngx_log_debug_http!(request, "ngx-inference:   - CA file: {:?}", ca_file);

            let mut tls_config = ClientTlsConfig::new().domain_name(&domain);

            // Use custom CA certificate if provided, otherwise use system roots
            if let Some(ca_path) = ca_file {
                ngx_log_debug_http!(request, "ngx-inference: Loading CA certificate from: {}", ca_path);

                // Read the CA certificate file
                let ca_cert = std::fs::read_to_string(ca_path).map_err(|e| {
                    format!("Failed to read CA certificate file '{}': {}", ca_path, e)
                })?;

                // Add the CA certificate to the TLS config
                tls_config =
                    tls_config.ca_certificate(tonic::transport::Certificate::from_pem(&ca_cert));
                ngx_log_debug_http!(request, "ngx-inference: Custom CA certificate loaded successfully");
            } else {
                tls_config = tls_config.with_enabled_roots();
                ngx_log_debug_http!(request, "ngx-inference: Using system CA certificate roots");
            }

            ngx_log_debug_http!(request, "ngx-inference: Building TLS config");
            ngx_log_debug_http!(request, "ngx-inference: Attempting gRPC connection...");

            let tls_result = channel_builder.tls_config(tls_config).map_err(|e| {
                ngx_log_debug_http!(request, "ngx-inference: TLS config failed: {}", e);
                format!("tls config error: {e}")
            })?;

            let connect_result = tls_result.connect().await;

            match &connect_result {
                Ok(_) => {
                    ngx_log_debug_http!(request, "ngx-inference: TLS connection established");
                }
                Err(e) => {
                    ngx_log_debug_http!(request, "ngx-inference: TLS connection failed: {}", e);
                    ngx_log_debug_http!(request, "ngx-inference:   - Error type: {:?}", e);
                    ngx_log_debug_http!(request, "ngx-inference:   - Endpoint: {}", endpoint);
                    ngx_log_debug_http!(request, "ngx-inference:   - Domain: {}", domain);

                    // Additional diagnostic information
                    ngx_log_debug_http!(request, "ngx-inference: Connection failure diagnostics:");
                    let error_str = format!("{}", e);
                    if error_str.contains("certificate") {
                        ngx_log_debug_http!(request, "ngx-inference:   - Certificate-related error detected");
                    }
                    if error_str.contains("hostname") {
                        ngx_log_debug_http!(request, "ngx-inference:   - Hostname verification error detected");
                    }
                    if error_str.contains("trust") {
                        ngx_log_debug_http!(request, "ngx-inference:   - Trust/CA error detected");
                    }
                }
            }

            connect_result.map_err(|e| {
                format!(
                    "connect error (endpoint: {}, domain: {}): {e}",
                    endpoint, domain
                )
            })?
        } else {
            // No TLS
            channel_builder
                .connect()
                .await
                .map_err(|e| format!("connect error: {e}"))?
        };

        let mut client = ExternalProcessorClient::new(channel);

        ngx_log_debug_http!(request, "ngx-inference: gRPC client created successfully");
        ngx_log_debug_http!(
            request,
            "ngx-inference: Preparing EPP request with {} headers",
            headers.len()
        );

        // EPP: For headers-only exchange, we still need to indicate body mode
        // but we mark end_of_stream=true on headers to indicate no body follows
        let proto_cfg = ProtocolConfiguration {
            request_body_mode: BodySendMode::None as i32,
            response_body_mode: BodySendMode::None as i32,
            send_body_without_waiting_for_header_response: false,
        };

        // Build HeaderMap from provided request headers.
        let mut header_entries: Vec<envoy::config::core::v3::HeaderValue> = Vec::new();
        for (k, v) in headers {
            header_entries.push(envoy::config::core::v3::HeaderValue {
                key: k,
                value: v,
                raw_value: Vec::new(),
            });
        }
        let header_map = HeaderMap {
            headers: header_entries,
        };

        // Build metadata_context for EPP routing metadata
        let metadata_context = {
            use prost_types::Struct;
            use std::collections::BTreeMap;
            let mut filter_metadata = std::collections::HashMap::new();

            // Add empty metadata structure for EPP to populate
            // EPP will use this for routing decisions
            let metadata_struct = Struct {
                fields: BTreeMap::new(),
            };
            filter_metadata.insert("envoy.lb".to_string(), metadata_struct);

            Some(envoy::config::core::v3::Metadata {
                filter_metadata,
                typed_filter_metadata: std::collections::HashMap::new(),
            })
        };

        let req_headers = HttpHeaders {
            headers: Some(header_map),
            attributes: std::collections::HashMap::new(),
            end_of_stream: true, // No body follows for headers-only exchange
        };

        use envoy::service::ext_proc::v3::processing_request;
        let headers_msg = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context,
            attributes: std::collections::HashMap::new(),
            observability_mode: false,
            protocol_config: Some(proto_cfg),
        };

        let outbound = tokio_stream::iter(vec![headers_msg]);

        ngx_log_debug_http!(request, "ngx-inference: Making gRPC process() call...");
        let start_time = std::time::Instant::now();

        let process_result = client.process(outbound).await;

        match &process_result {
            Ok(_) => {
                let duration = start_time.elapsed();
                ngx_log_debug_http!(request, "ngx-inference: gRPC process() call completed in {:?}", duration);
            }
            Err(e) => {
                let duration = start_time.elapsed();
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: gRPC process() call failed after {:?}: {}",
                    duration, e
                );
                ngx_log_debug_http!(request, "ngx-inference:   - Status: {:?}", e.code());
                ngx_log_debug_http!(request, "ngx-inference:   - Message: {}", e.message());
                ngx_log_debug_http!(request, "ngx-inference:   - Details: {:?}", e.details());
            }
        }

        let mut inbound = process_result
            .map_err(|e| format!("rpc error: {e}"))?
            .into_inner();

        let next = if timeout_ms == 0 {
            inbound.message().await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                inbound.message(),
            )
            .await
            {
                Ok(res) => res,
                Err(_) => return Ok(None),
            }
        };

        match next {
            Ok(Some(resp)) => {
                ngx_log_debug_http!(request, "ngx-inference: Received EPP response: {:?}", resp);

                if let Some(val) = parse_response_for_header(request, &resp, &target_key_lower) {
                    ngx_log_debug_http!(request, "ngx-inference: Found header '{}' with value: {}", header_name, val);
                    return Ok(Some(val));
                } else {
                    ngx_log_debug_http!(request, "ngx-inference: Header '{}' not found in response", header_name);
                }
            }
            Ok(None) => {
                ngx_log_debug_http!(request, "ngx-inference: EPP response stream closed");
            }
            Err(e) => {
                ngx_log_debug_http!(request, "ngx-inference: EPP stream receive error: {}", e);
                return Err(format!("stream recv error: {e}"));
            }
        }

        // Continue reading additional responses until stream ends or we find the header.
        loop {
            match inbound.message().await {
                Ok(Some(resp)) => {
                    ngx_log_debug_http!(request, "ngx-inference: Received additional EPP response: {:?}", resp);

                    if let Some(val) = parse_response_for_header(request, &resp, &target_key_lower) {
                        ngx_log_debug_http!(
                            request,
                            "ngx-inference: Found header '{}' with value in additional response: {}",
                            header_name, val
                        );
                        return Ok(Some(val));
                    } else {
                        ngx_log_debug_http!(
                            request,
                            "ngx-inference: Header '{}' not found in additional response",
                            header_name
                        );
                    }
                }
                Ok(None) => {
                    ngx_log_debug_http!(request, "ngx-inference: EPP response stream ended");
                    break;
                }
                Err(e) => {
                    ngx_log_debug_http!(request, "ngx-inference: EPP stream receive error in continuation: {}", e);
                    return Err(format!("stream recv error: {e}"));
                }
            }
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: EPP processing completed, header '{}' not found in any response",
            header_name
        );
        Ok(None)
    })
}
