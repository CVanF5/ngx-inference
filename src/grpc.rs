//! gRPC client implementation for Envoy ExternalProcessor (ext-proc) protocol.
//!
//! This module implements EPP (Endpoint Picker Processor) for Gateway API Inference Extension:
//! - Headers-only exchange for upstream endpoint selection
//!
//! The implementation follows the Gateway API Inference Extension specification.

use crate::protos::envoy;

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
    mutation: &envoy::service::ext_proc::v3::HeaderMutation,
    target_key_lower: &str,
) -> Option<String> {
    eprintln!(
        "DEBUG: Searching for header '{}' in mutation with {} headers",
        target_key_lower,
        mutation.set_headers.len()
    );

    // Log all available headers for debugging
    for (i, hvo) in mutation.set_headers.iter().enumerate() {
        if let Some(hdr) = &hvo.header {
            eprintln!(
                "DEBUG: Header[{}]: key='{}', value='{}', raw_value_len={}",
                i,
                hdr.key,
                hdr.value,
                hdr.raw_value.len()
            );
        }
    }

    for hvo in &mutation.set_headers {
        if let Some(hdr) = &hvo.header {
            eprintln!(
                "DEBUG: Comparing '{}' == '{}' (ignore case)",
                hdr.key, target_key_lower
            );
            // Keys are lower-cased in HttpHeaders; we compare ASCII-case-insensitively just in case.
            if hdr.key.eq_ignore_ascii_case(target_key_lower) {
                if !hdr.value.is_empty() {
                    let value = hdr.value.clone();
                    eprintln!("DEBUG: Found matching header with value: '{}'", value);
                    return Some(value);
                }
                if !hdr.raw_value.is_empty() {
                    let value = String::from_utf8_lossy(&hdr.raw_value).to_string();
                    eprintln!("DEBUG: Found matching header with raw_value: '{}'", value);
                    return Some(value);
                }
                eprintln!("DEBUG: Found matching header key but no value");
            }
        }
    }

    eprintln!(
        "DEBUG: Target header '{}' not found in header mutation",
        target_key_lower
    );
    None
}

fn parse_response_for_header(resp: &ProcessingResponse, target_key_lower: &str) -> Option<String> {
    use envoy::service::ext_proc::v3::processing_response;

    eprintln!("DEBUG: Parsing response for header '{}'", target_key_lower);

    match &resp.response {
        Some(processing_response::Response::RequestHeaders(hdrs)) => {
            eprintln!("DEBUG: Processing RequestHeaders response");
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    eprintln!(
                        "DEBUG: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(hm, target_key_lower);
                } else {
                    eprintln!("DEBUG: No header mutation in RequestHeaders");
                }
            } else {
                eprintln!("DEBUG: No common response in RequestHeaders");
            }
        }
        Some(processing_response::Response::ResponseHeaders(hdrs)) => {
            eprintln!("DEBUG: Processing ResponseHeaders response");
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    eprintln!(
                        "DEBUG: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(hm, target_key_lower);
                } else {
                    eprintln!("DEBUG: No header mutation in ResponseHeaders");
                }
            } else {
                eprintln!("DEBUG: No common response in ResponseHeaders");
            }
        }
        Some(processing_response::Response::RequestBody(body)) => {
            eprintln!("DEBUG: Processing RequestBody response");
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    eprintln!(
                        "DEBUG: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(hm, target_key_lower);
                } else {
                    eprintln!("DEBUG: No header mutation in RequestBody");
                }
            } else {
                eprintln!("DEBUG: No common response in RequestBody");
            }
        }
        Some(processing_response::Response::ResponseBody(body)) => {
            eprintln!("DEBUG: Processing ResponseBody response");
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    eprintln!(
                        "DEBUG: Found header mutation with {} headers",
                        hm.set_headers.len()
                    );
                    return extract_header_from_mutation(hm, target_key_lower);
                } else {
                    eprintln!("DEBUG: No header mutation in ResponseBody");
                }
            } else {
                eprintln!("DEBUG: No common response in ResponseBody");
            }
        }
        Some(processing_response::Response::RequestTrailers(tr)) => {
            eprintln!("DEBUG: Processing RequestTrailers response");
            if let Some(hm) = &tr.header_mutation {
                eprintln!(
                    "DEBUG: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(hm, target_key_lower);
            } else {
                eprintln!("DEBUG: No header mutation in RequestTrailers");
            }
        }
        Some(processing_response::Response::ResponseTrailers(tr)) => {
            eprintln!("DEBUG: Processing ResponseTrailers response");
            if let Some(hm) = &tr.header_mutation {
                eprintln!(
                    "DEBUG: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(hm, target_key_lower);
            } else {
                eprintln!("DEBUG: No header mutation in ResponseTrailers");
            }
        }
        Some(processing_response::Response::ImmediateResponse(ir)) => {
            eprintln!(
                "DEBUG: Processing ImmediateResponse (status: {:?})",
                ir.status
            );
            if let Some(hm) = &ir.headers {
                eprintln!(
                    "DEBUG: Found header mutation with {} headers",
                    hm.set_headers.len()
                );
                return extract_header_from_mutation(hm, target_key_lower);
            } else {
                eprintln!("DEBUG: No header mutation in ImmediateResponse");
            }
        }
        None => {
            eprintln!("DEBUG: Response has no content (None)");
        }
    }

    eprintln!("DEBUG: No matching header found in response");
    None
}

/// EPP: Request headers and body exchange for upstream endpoint selection.
///
/// Returns Ok(Some(value)) if the ext-proc service replies with a header mutation
/// for the specified header name; Ok(None) if not present; Err(...) on transport-level errors.
pub fn epp_headers_blocking(
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

            eprintln!("DEBUG: TLS Configuration:");
            eprintln!("  - Endpoint: {}", endpoint);
            eprintln!("  - URI: {}", uri);
            eprintln!("  - Extracted domain: '{}'", domain);
            eprintln!("  - CA file: {:?}", ca_file);

            let mut tls_config = ClientTlsConfig::new().domain_name(&domain);

            // Use custom CA certificate if provided, otherwise use system roots
            if let Some(ca_path) = ca_file {
                eprintln!("DEBUG: Loading CA certificate from: {}", ca_path);

                // Read the CA certificate file
                let ca_cert = std::fs::read_to_string(ca_path).map_err(|e| {
                    format!("Failed to read CA certificate file '{}': {}", ca_path, e)
                })?;

                // Add the CA certificate to the TLS config
                tls_config =
                    tls_config.ca_certificate(tonic::transport::Certificate::from_pem(&ca_cert));
                eprintln!("DEBUG: Custom CA certificate loaded successfully");
            } else {
                tls_config = tls_config.with_enabled_roots();
                eprintln!("DEBUG: Using system CA certificate roots");
            }

            eprintln!("DEBUG: Building TLS config");
            eprintln!("DEBUG: Attempting gRPC connection...");

            let tls_result = channel_builder.tls_config(tls_config).map_err(|e| {
                eprintln!("ERROR: TLS config failed: {}", e);
                format!("tls config error: {e}")
            })?;

            let connect_result = tls_result.connect().await;

            match &connect_result {
                Ok(_) => eprintln!("SUCCESS: TLS connection established"),
                Err(e) => {
                    eprintln!("ERROR: TLS connection failed: {}", e);
                    eprintln!("  - Error type: {:?}", e);
                    eprintln!("  - Endpoint: {}", endpoint);
                    eprintln!("  - Domain: {}", domain);

                    // Additional diagnostic information
                    eprintln!("DEBUG: Connection failure diagnostics:");
                    let error_str = format!("{}", e);
                    if error_str.contains("certificate") {
                        eprintln!("  - Certificate-related error detected");
                    }
                    if error_str.contains("hostname") {
                        eprintln!("  - Hostname verification error detected");
                    }
                    if error_str.contains("trust") {
                        eprintln!("  - Trust/CA error detected");
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

        eprintln!("DEBUG: gRPC client created successfully");
        eprintln!(
            "DEBUG: Preparing EPP request with {} headers",
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

        eprintln!("DEBUG: Making gRPC process() call...");
        let start_time = std::time::Instant::now();

        let process_result = client.process(outbound).await;

        match &process_result {
            Ok(_) => {
                let duration = start_time.elapsed();
                eprintln!("SUCCESS: gRPC process() call completed in {:?}", duration);
            }
            Err(e) => {
                let duration = start_time.elapsed();
                eprintln!(
                    "ERROR: gRPC process() call failed after {:?}: {}",
                    duration, e
                );
                eprintln!("  - Status: {:?}", e.code());
                eprintln!("  - Message: {}", e.message());
                eprintln!("  - Details: {:?}", e.details());
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
                eprintln!("DEBUG: Received EPP response: {:?}", resp);

                if let Some(val) = parse_response_for_header(&resp, &target_key_lower) {
                    eprintln!("DEBUG: Found header '{}' with value: {}", header_name, val);
                    return Ok(Some(val));
                } else {
                    eprintln!("DEBUG: Header '{}' not found in response", header_name);
                }
            }
            Ok(None) => {
                eprintln!("DEBUG: EPP response stream closed");
            }
            Err(e) => {
                eprintln!("ERROR: EPP stream receive error: {}", e);
                return Err(format!("stream recv error: {e}"));
            }
        }

        // Continue reading additional responses until stream ends or we find the header.
        loop {
            match inbound.message().await {
                Ok(Some(resp)) => {
                    eprintln!("DEBUG: Received additional EPP response: {:?}", resp);

                    if let Some(val) = parse_response_for_header(&resp, &target_key_lower) {
                        eprintln!(
                            "DEBUG: Found header '{}' with value in additional response: {}",
                            header_name, val
                        );
                        return Ok(Some(val));
                    } else {
                        eprintln!(
                            "DEBUG: Header '{}' not found in additional response",
                            header_name
                        );
                    }
                }
                Ok(None) => {
                    eprintln!("DEBUG: EPP response stream ended");
                    break;
                }
                Err(e) => {
                    eprintln!("ERROR: EPP stream receive error in continuation: {}", e);
                    return Err(format!("stream recv error: {e}"));
                }
            }
        }

        eprintln!(
            "DEBUG: EPP processing completed, header '{}' not found in any response",
            header_name
        );
        Ok(None)
    })
}
