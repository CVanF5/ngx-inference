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

// Helper macro for info-level logging in gRPC operations
#[allow(unused_macros)]
macro_rules! ngx_log_info_http {
    ($request:expr, $($arg:tt)*) => {
        unsafe {
            let msg = format!($($arg)*);
            let c_msg = std::ffi::CString::new(msg).unwrap();
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_INFO as ngx::ffi::ngx_uint_t,
                ($request.connection().as_ref().unwrap().log),
                0,
                c_msg.as_ptr(),
            );
        }
    };
}

// Helper macro for warning-level logging in gRPC operations
#[allow(unused_macros)]
macro_rules! ngx_log_warn_http {
    ($request:expr, $($arg:tt)*) => {
        unsafe {
            let msg = format!($($arg)*);
            let c_msg = std::ffi::CString::new(msg).unwrap();
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                ($request.connection().as_ref().unwrap().log),
                0,
                c_msg.as_ptr(),
            );
        }
    };
}

// Helper macro for error-level logging in gRPC operations
#[allow(unused_macros)]
macro_rules! ngx_log_error_http {
    ($request:expr, $($arg:tt)*) => {
        unsafe {
            let msg = format!($($arg)*);
            let c_msg = std::ffi::CString::new(msg).unwrap();
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                ($request.connection().as_ref().unwrap().log),
                0,
                c_msg.as_ptr(),
            );
        }
    };
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
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

fn parse_response_for_header_async(
    resp: &ProcessingResponse,
    target_key_lower: &str,
) -> Option<String> {
    use envoy::service::ext_proc::v3::processing_response;

    match &resp.response {
        Some(processing_response::Response::RequestHeaders(hdrs)) => {
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation_async(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::ResponseHeaders(hdrs)) => {
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation_async(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestBody(body)) => {
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation_async(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::ResponseBody(body)) => {
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation_async(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestTrailers(tr)) => {
            if let Some(hm) = &tr.header_mutation {
                return extract_header_from_mutation_async(hm, target_key_lower);
            }
        }
        Some(processing_response::Response::ResponseTrailers(tr)) => {
            if let Some(hm) = &tr.header_mutation {
                return extract_header_from_mutation_async(hm, target_key_lower);
            }
        }
        Some(processing_response::Response::ImmediateResponse(ir)) => {
            if let Some(hm) = &ir.headers {
                return extract_header_from_mutation_async(hm, target_key_lower);
            }
        }
        None => {}
    }

    None
}

fn extract_header_from_mutation_async(
    mutation: &envoy::service::ext_proc::v3::HeaderMutation,
    target_key_lower: &str,
) -> Option<String> {
    for hvo in &mutation.set_headers {
        if let Some(hdr) = &hvo.header {
            if hdr.key.eq_ignore_ascii_case(target_key_lower) {
                if !hdr.value.is_empty() {
                    return Some(hdr.value.clone());
                }
                if !hdr.raw_value.is_empty() {
                    return Some(String::from_utf8_lossy(&hdr.raw_value).to_string());
                }
            }
        }
    }
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
    // Wrap the entire EPP operation in a panic handler to prevent worker crashes
    let result = std::panic::catch_unwind(|| {
        let target_key_lower = header_name.to_ascii_lowercase();
        let uri = normalize_endpoint(endpoint, use_tls);

        // Don't log from within async context - create copies of data first
        let endpoint_copy = endpoint.to_string();
        let use_tls_copy = use_tls;

        get_runtime().block_on(async move {
            let channel_builder =
                Channel::from_shared(uri.clone()).map_err(|e| format!("channel error: {e}"))?;

            // Build the channel with appropriate TLS configuration
            let channel = if use_tls_copy {
                // SECURE MODE: Configure TLS with custom CA if provided, otherwise use system roots
                use tonic::transport::ClientTlsConfig;

                // Extract domain from endpoint for TLS verification
                let domain = if let Some(colon_pos) = endpoint_copy.rfind(':') {
                    endpoint_copy[..colon_pos].to_string()
                } else {
                    endpoint_copy.clone()
                };

                let mut tls_config = ClientTlsConfig::new().domain_name(&domain);

                // Use custom CA certificate if provided, otherwise use system roots
                if let Some(ca_path) = ca_file {
                    // Read the CA certificate file
                    let ca_cert = std::fs::read_to_string(ca_path).map_err(|e| {
                        format!("Failed to read CA certificate file '{}': {}", ca_path, e)
                    })?;

                    // Add the CA certificate to the TLS config
                    tls_config = tls_config
                        .ca_certificate(tonic::transport::Certificate::from_pem(&ca_cert));
                } else {
                    tls_config = tls_config.with_enabled_roots();
                }

                let tls_result = channel_builder
                    .tls_config(tls_config)
                    .map_err(|e| format!("tls config error: {e}"))?;

                let connect_result = tls_result.connect().await;

                connect_result.map_err(|e| {
                    format!(
                        "connect error (endpoint: {}, domain: {}): {e}",
                        endpoint_copy, domain
                    )
                })?
            } else {
                // PLAINTEXT MODE: No TLS configuration
                channel_builder
                    .connect()
                    .await
                    .map_err(|e| format!("connect error: {e}"))?
            };

            let mut client = ExternalProcessorClient::new(channel);

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

            let process_result = client.process(outbound).await;

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
                    if let Some(val) = parse_response_for_header(request, &resp, &target_key_lower)
                    {
                        return Ok(Some(val));
                    }
                }
                Ok(None) => {
                    // EPP response stream closed, no header provided
                }
                Err(e) => {
                    return Err(format!("stream recv error: {e}"));
                }
            }

            // Continue reading additional responses until stream ends or we find the header.
            loop {
                match inbound.message().await {
                    Ok(Some(resp)) => {
                        if let Some(val) =
                            parse_response_for_header(request, &resp, &target_key_lower)
                        {
                            return Ok(Some(val));
                        }
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        return Err(format!("stream recv error: {e}"));
                    }
                }
            }

            Ok(None)
        })
    });

    // Handle panic recovery
    match result {
        Ok(grpc_result) => {
            match &grpc_result {
                Ok(Some(upstream)) => {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: EPP selected upstream: {}",
                        upstream
                    );
                }
                Ok(None) => {
                    ngx_log_debug_http!(request, "ngx-inference: EPP returned no upstream");
                }
                Err(e) => {
                    ngx_log_error_http!(request, "ngx-inference: EPP failed: {}", e);
                }
            }
            grpc_result
        }
        Err(_panic_info) => {
            ngx_log_error_http!(
                request,
                "ngx-inference: EPP gRPC operation panicked, endpoint: {}",
                endpoint
            );
            Err("EPP gRPC operation panicked".to_string())
        }
    }
}

/// EPP: Async headers exchange - DEPRECATED AND UNSAFE
///
/// ⚠️  WARNING: This function is UNUSED and should NOT be called.
/// ⚠️  It causes NGINX worker crashes due to threading model violations.
///
/// PROBLEM: This function spawns background threads that call NGINX functions
/// (ngx_http_core_run_phases), which violates NGINX's single-threaded event loop
/// model and results in segmentation faults (SIGSEGV signal 11).
///
/// ✅ USE INSTEAD: epp_headers_blocking() - safe blocking implementation
///
/// This function remains in the codebase only for reference. It demonstrates
/// why naive async approaches don't work with NGINX modules.
#[allow(clippy::too_many_arguments)]
pub fn epp_headers_async<F>(
    request_ptr: *mut ngx::ffi::ngx_http_request_t,
    endpoint: String,
    timeout_ms: u64,
    header_name: String,
    headers: Vec<(String, String)>,
    use_tls: bool,
    ca_file: Option<String>,
    completion_callback: F,
) where
    F: FnOnce(*mut ngx::ffi::ngx_http_request_t, Result<Option<String>, String>) + Send + 'static,
{
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(&endpoint, use_tls);

    // Convert to usize to make it Send-safe across threads
    let request_ptr_addr = request_ptr as usize;

    // Log the start of async operation (we can't safely log from async context)
    // Note: This logging happens before we enter the async context

    // Spawn the async operation without blocking
    let rt = get_runtime();
    rt.spawn(async move {
        let result = async move {
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

                // Logging not available in async context - would need to pass request context safely
                let mut tls_config = ClientTlsConfig::new().domain_name(&domain);

                // Use custom CA certificate if provided, otherwise use system roots
                if let Some(ca_path) = ca_file {
                    // Read the CA certificate file
                    let ca_cert = std::fs::read_to_string(ca_path)
                        .map_err(|e| format!("Failed to read CA certificate file: {}", e))?;

                    // Add the CA certificate to the TLS config
                    tls_config = tls_config
                        .ca_certificate(tonic::transport::Certificate::from_pem(&ca_cert));
                } else {
                    tls_config = tls_config.with_enabled_roots();
                }

                let tls_result = channel_builder
                    .tls_config(tls_config)
                    .map_err(|e| format!("tls config error: {e}"))?;

                tls_result.connect().await.map_err(|e| {
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

            let process_result = client.process(outbound).await;
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
                    // We can't safely log from async context without request reference
                    // The callback will handle logging instead
                    if let Some(val) = parse_response_for_header_async(&resp, &target_key_lower) {
                        return Ok(Some(val));
                    }
                }
                Ok(None) => {
                    // Stream closed
                }
                Err(e) => {
                    return Err(format!("stream recv error: {e}"));
                }
            }

            // Continue reading additional responses until stream ends or we find the header.
            loop {
                match inbound.message().await {
                    Ok(Some(resp)) => {
                        if let Some(val) = parse_response_for_header_async(&resp, &target_key_lower)
                        {
                            return Ok(Some(val));
                        }
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        return Err(format!("stream recv error: {e}"));
                    }
                }
            }

            Ok(None)
        }
        .await;

        // Log completion status before calling callback
        // We'll log the final result in the callback where we have request context

        // Call the completion callback with the result
        completion_callback(
            request_ptr_addr as *mut ngx::ffi::ngx_http_request_t,
            result,
        );
    });
}

/// Make the runtime accessible to other modules
pub fn get_tokio_runtime() -> &'static tokio::runtime::Runtime {
    get_runtime()
}

/// Internal async EPP function for testing and potential future use.
/// This is thread-safe but currently unused in production.
/// The main implementation uses epp_headers_blocking() instead.
pub async fn epp_headers_blocking_internal(
    endpoint: &str,
    timeout_ms: u64,
    header_name: &str,
    headers: Vec<(String, String)>,
    use_tls: bool,
    ca_file: Option<&str>,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint, use_tls);

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

        let mut tls_config = ClientTlsConfig::new().domain_name(&domain);

        // Use custom CA certificate if provided, otherwise use system roots
        if let Some(ca_path) = ca_file {
            // Read the CA certificate file
            let ca_cert = std::fs::read_to_string(ca_path)
                .map_err(|e| format!("Failed to read CA certificate file '{}': {}", ca_path, e))?;

            // Add the CA certificate to the TLS config
            tls_config =
                tls_config.ca_certificate(tonic::transport::Certificate::from_pem(&ca_cert));
        } else {
            tls_config = tls_config.with_enabled_roots();
        }

        let tls_result = channel_builder
            .tls_config(tls_config)
            .map_err(|e| format!("tls config error: {e}"))?;

        tls_result.connect().await.map_err(|e| {
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

    let process_result = client.process(outbound).await;
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
            if let Some(val) = parse_response_for_header_async(&resp, &target_key_lower) {
                return Ok(Some(val));
            }
        }
        Ok(None) => {
            // Stream closed
        }
        Err(e) => {
            return Err(format!("stream recv error: {e}"));
        }
    }

    // Continue reading additional responses until stream ends or we find the header.
    loop {
        match inbound.message().await {
            Ok(Some(resp)) => {
                if let Some(val) = parse_response_for_header_async(&resp, &target_key_lower) {
                    return Ok(Some(val));
                }
            }
            Ok(None) => {
                break;
            }
            Err(e) => {
                return Err(format!("stream recv error: {e}"));
            }
        }
    }

    Ok(None)
}
