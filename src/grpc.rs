//! gRPC client implementation for Envoy ExternalProcessor (ext-proc).
//!
//! This module implements minimal bidirectional streaming interactions needed
//! by EPP (Endpoint Picker Processor) and BBR (Body-Based Routing).
//!
//! It uses tonic to connect to an ext-proc server and exchanges
//! ProcessingRequest/ProcessingResponse messages. The implementation here focuses
//! on reading header mutations returned by the server and extracting a specific
//! header value to be injected back into the original NGINX request.
//!
//! Notes:
//! - We create a lightweight Tokio runtime per call to avoid requiring a global
//!   runtime, keeping integration with ngx-rust simple.
//! - We send a headers message and a ProtocolConfiguration on the first request,
//!   indicating the desired body handling mode (NONE for EPP, STREAMED for BBR).
//! - For BBR, this initial version does not stream the actual body; it can be
//!   extended to stream chunks from the NGINX request body when integrating deeper
//!   with the ngx-rust request APIs.

use crate::protos::envoy;

use std::collections::HashMap;

use tonic::transport::Channel;

type ExternalProcessorClient<T> =
    envoy::service::ext_proc::v3::external_processor_client::ExternalProcessorClient<T>;

type ProcessingRequest = envoy::service::ext_proc::v3::ProcessingRequest;
type ProcessingResponse = envoy::service::ext_proc::v3::ProcessingResponse;

type ProtocolConfiguration = envoy::service::ext_proc::v3::ProtocolConfiguration;
type BodySendMode =
    envoy::extensions::filters::http::ext_proc::v3::processing_mode::BodySendMode;

type HttpHeaders = envoy::service::ext_proc::v3::HttpHeaders;
type HttpBody = envoy::service::ext_proc::v3::HttpBody;
type HeaderMap = envoy::config::core::v3::HeaderMap;
// type HeaderValueOption = envoy::config::core::v3::HeaderValueOption;

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

fn parse_response_for_header(
    resp: &ProcessingResponse,
    target_key_lower: &str,
) -> Option<String> {
    use envoy::service::ext_proc::v3::processing_response;

    match &resp.response {
        Some(processing_response::Response::RequestHeaders(hdrs)) |
        Some(processing_response::Response::ResponseHeaders(hdrs)) => {
            if let Some(common) = &hdrs.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestBody(body)) |
        Some(processing_response::Response::ResponseBody(body)) => {
            if let Some(common) = &body.response {
                if let Some(hm) = &common.header_mutation {
                    return extract_header_from_mutation(hm, target_key_lower);
                }
            }
        }
        Some(processing_response::Response::RequestTrailers(tr)) |
        Some(processing_response::Response::ResponseTrailers(tr)) => {
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

/// EPP: Headers-only exchange to obtain upstream selection header.
///
/// Returns Ok(Some(value)) if the ext-proc service replies with a header mutation
/// for "X-Inference-Upstream"; Ok(None) if not present; Err(...) on transport-level errors.
pub fn epp_headers_blocking(
    endpoint: &str,
    timeout_ms: u64,
) -> Result<Option<String>, String> {
    let uri = normalize_endpoint(endpoint);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime build error: {e}"))?;

    rt.block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        // Prepare ProtocolConfiguration: no body for EPP.
        let proto_cfg = ProtocolConfiguration {
            request_body_mode: BodySendMode::None as i32,
            response_body_mode: BodySendMode::None as i32,
            send_body_without_waiting_for_header_response: false,
        };

        // Minimal headers: empty map; end_of_stream true (no body).
        let headers = HeaderMap { headers: Vec::new() };
        let req_headers = HttpHeaders {
            headers: Some(headers),
            attributes: HashMap::new(),
            end_of_stream: true,
        };

        // First request carries both headers and protocol_config.
        use envoy::service::ext_proc::v3::processing_request;
        let first = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context: None,
            attributes: HashMap::new(),
            observability_mode: false,
            protocol_config: Some(proto_cfg),
        };

        // Build outbound stream with a single item.
        let outbound = tokio_stream::iter(vec![first]);

        // Apply timeout on receiving responses if requested.
        let mut inbound = client
            .process(outbound)
            .await
            .map_err(|e| format!("rpc error: {e}"))?
            .into_inner();

        // Optional timeout for first response
        let next = if timeout_ms == 0 {
            inbound.message().await
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inbound.message()).await {
                Ok(res) => res,
                Err(_) => return Ok(None),
            }
        };

        match next {
            Ok(Some(resp)) => {
                if let Some(val) = parse_response_for_header(&resp, "x-inference-upstream") {
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
                    if let Some(val) = parse_response_for_header(&resp, "x-inference-upstream") {
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

pub fn epp_headers_blocking_with_header(
    endpoint: &str,
    timeout_ms: u64,
    header_name: &str,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime build error: {e}"))?;

    rt.block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        let proto_cfg = ProtocolConfiguration {
            request_body_mode: BodySendMode::None as i32,
            response_body_mode: BodySendMode::None as i32,
            send_body_without_waiting_for_header_response: false,
        };

        let headers = HeaderMap { headers: Vec::new() };
        let req_headers = HttpHeaders {
            headers: Some(headers),
            attributes: HashMap::new(),
            end_of_stream: true,
        };

        use envoy::service::ext_proc::v3::processing_request;
        let first = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context: None,
            attributes: HashMap::new(),
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
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inbound.message()).await {
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

pub fn epp_headers_blocking_with_headers(
    endpoint: &str,
    timeout_ms: u64,
    header_name: &str,
    headers: Vec<(String, String)>,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime build error: {e}"))?;

    rt.block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        // No body for EPP.
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
        let header_map = HeaderMap { headers: header_entries };

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
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inbound.message()).await {
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

/// BBR: Initiate streaming mode and read a returned header (model name).
///
/// Returns Ok(Some(value)) if the ext-proc service replies with a header mutation
/// for the provided header_name; Ok(None) if not present; Err(...) on transport-level errors.
///
/// This initial implementation sends a headers message and protocol_config indicating
/// STREAMED mode, followed optionally by an empty body chunk. It can be extended to
/// stream actual body chunks from the NGINX request when available.
pub fn bbr_stream_blocking(
    endpoint: &str,
    body_len: usize,
    _chunk_size: usize,
    header_name: &str,
    timeout_ms: u64,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime build error: {e}"))?;

    rt.block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        let proto_cfg = ProtocolConfiguration {
            request_body_mode: BodySendMode::Streamed as i32,
            response_body_mode: BodySendMode::None as i32,
            send_body_without_waiting_for_header_response: true,
        };

        let headers = HeaderMap { headers: Vec::new() };
        let req_headers = HttpHeaders {
            headers: Some(headers),
            attributes: HashMap::new(),
            end_of_stream: body_len == 0,
        };

        use envoy::service::ext_proc::v3::processing_request;
        let first = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context: None,
            attributes: HashMap::new(),
            observability_mode: false,
            protocol_config: Some(proto_cfg),
        };

        // Optionally send a single empty body chunk if body_len > 0 is unknown.
        let mut items = vec![first];

        if body_len > 0 {
            let body = HttpBody {
                body: Vec::new(), // placeholder: integrate actual request body later
                end_of_stream: true,
            };
            let second = ProcessingRequest {
                request: Some(processing_request::Request::RequestBody(body)),
                metadata_context: None,
                attributes: HashMap::new(),
                observability_mode: false,
                protocol_config: None,
            };
            items.push(second);
        }

        let outbound = tokio_stream::iter(items);

        let mut inbound = client
            .process(outbound)
            .await
            .map_err(|e| format!("rpc error: {e}"))?
            .into_inner();

        // Read responses until stream ends, searching for the header mutation.
        loop {
            let next = if timeout_ms == 0 {
                inbound.message().await
            } else {
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inbound.message()).await {
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
                Ok(None) => break,
                Err(e) => return Err(format!("stream recv error: {e}")),
            }
        }

        Ok(None)
    })
}

/// BBR: Stream actual request body bytes to ext-proc and read returned header (model name).
///
/// - Sends RequestHeaders first with STREAMED mode configured
/// - Then streams the provided body in chunks (chunk_size, default 64 KiB if 0)
/// - Returns the value of `header_name` from a HeaderMutation response if present
pub fn bbr_stream_blocking_with_body(
    endpoint: &str,
    body: &[u8],
    chunk_size: usize,
    header_name: &str,
    timeout_ms: u64,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime build error: {e}"))?;

    rt.block_on(async move {
        let channel = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("connect error: {e}"))?;

        let mut client = ExternalProcessorClient::new(channel);

        let proto_cfg = ProtocolConfiguration {
            request_body_mode: BodySendMode::Streamed as i32,
            response_body_mode: BodySendMode::None as i32,
            send_body_without_waiting_for_header_response: true,
        };

        // First message: headers with end_of_stream depending on whether body is empty.
        let headers = HeaderMap { headers: Vec::new() };
        let req_headers = HttpHeaders {
            headers: Some(headers),
            attributes: HashMap::new(),
            end_of_stream: body.is_empty(),
        };

        use envoy::service::ext_proc::v3::processing_request;
        let first = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(req_headers)),
            metadata_context: None,
            attributes: HashMap::new(),
            observability_mode: false,
            protocol_config: Some(proto_cfg),
        };

        // Prepare body chunks
        let cs = if chunk_size == 0 { 64 * 1024 } else { chunk_size };
        let mut items = Vec::with_capacity(1 + (body.len() / cs) + 1);
        items.push(first);

        if !body.is_empty() {
            let mut offset = 0usize;
            while offset < body.len() {
                let end = (offset + cs).min(body.len());
                let chunk = &body[offset..end];
                let body_msg = HttpBody {
                    body: chunk.to_vec(),
                    end_of_stream: end == body.len(),
                };
                let req = ProcessingRequest {
                    request: Some(processing_request::Request::RequestBody(body_msg)),
                    metadata_context: None,
                    attributes: HashMap::new(),
                    observability_mode: false,
                    protocol_config: None,
                };
                items.push(req);
                offset = end;
            }
        }

        let outbound = tokio_stream::iter(items);

        let mut inbound = client
            .process(outbound)
            .await
            .map_err(|e| format!("rpc error: {e}"))?
            .into_inner();

        // Read responses until stream ends, searching for the header mutation.
        loop {
            let next = if timeout_ms == 0 {
                inbound.message().await
            } else {
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inbound.message()).await {
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
                Ok(None) => break,
                Err(e) => return Err(format!("stream recv error: {e}")),
            }
        }

        Ok(None)
    })
}
