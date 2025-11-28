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

fn normalize_endpoint(endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{}", endpoint)
    }
}

fn extract_header_from_mutation(
    mutation: &envoy::service::ext_proc::v3::HeaderMutation,
    target_key_lower: &str,
) -> Option<String> {
    for hvo in &mutation.set_headers {
        if let Some(hdr) = &hvo.header {
            // Keys are lower-cased in HttpHeaders; we compare ASCII-case-insensitively just in case.
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

fn parse_response_for_header(resp: &ProcessingResponse, target_key_lower: &str) -> Option<String> {
    use envoy::service::ext_proc::v3::processing_response;

    match &resp.response {
        Some(processing_response::Response::RequestHeaders(hdrs))
        | Some(processing_response::Response::ResponseHeaders(hdrs)) => {
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestBody(body))
        | Some(processing_response::Response::ResponseBody(body)) => {
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestTrailers(tr))
        | Some(processing_response::Response::ResponseTrailers(tr)) => {
            if let Some(hm) = &tr.header_mutation {
                return extract_header_from_mutation(hm, target_key_lower);
            }
        }
        Some(processing_response::Response::ImmediateResponse(ir)) => {
            if let Some(hm) = &ir.headers {
                return extract_header_from_mutation(hm, target_key_lower);
            }
        }
        None => {}
    }

    None
}

/// EPP: Headers-only exchange for upstream endpoint selection.
///
/// Returns Ok(Some(value)) if the ext-proc service replies with a header mutation
/// for the specified header name; Ok(None) if not present; Err(...) on transport-level errors.
pub fn epp_headers_blocking(
    endpoint: &str,
    timeout_ms: u64,
    header_name: &str,
    headers: Vec<(String, String)>,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint);

    get_runtime().block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        // EPP uses headers-only mode - no body processing
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

        let req_headers = HttpHeaders {
            headers: Some(header_map),
            attributes: std::collections::HashMap::new(),
            end_of_stream: true,
        };

        use envoy::service::ext_proc::v3::processing_request;
        let first = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context: None,
            attributes: std::collections::HashMap::new(),
            observability_mode: false,
            protocol_config: Some(proto_cfg),
        };

        let outbound = tokio_stream::iter(vec![first]);

        let mut inbound = client
            .process(outbound)
            .await
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
                if let Some(val) = parse_response_for_header(&resp, &target_key_lower) {
                    return Ok(Some(val));
                }
            }
            Ok(None) => {} // stream closed
            Err(e) => return Err(format!("stream recv error: {e}")),
        }

        // Continue reading additional responses until stream ends or we find the header.
        loop {
            match inbound.message().await {
                Ok(Some(resp)) => {
                    if let Some(val) = parse_response_for_header(&resp, &target_key_lower) {
                        return Ok(Some(val));
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(format!("stream recv error: {e}")),
            }
        }

        Ok(None)
    })
}
