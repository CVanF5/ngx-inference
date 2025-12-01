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
    tls_insecure: bool,
) -> Result<Option<String>, String> {
    let target_key_lower = header_name.to_ascii_lowercase();
    let uri = normalize_endpoint(endpoint, use_tls);

    get_runtime().block_on(async move {
        let channel_builder = Channel::from_shared(uri.clone())
            .map_err(|e| format!("channel error: {e}"))?;

        // Build the channel with appropriate TLS configuration
        let channel = if use_tls && tls_insecure {
            // INSECURE MODE: Accept self-signed certificates
            // WARNING: Only for development/testing. Uses a no-op certificate verifier.
            use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
            use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
            use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
            use std::sync::Arc;
            
            // Minimal verifier that accepts any certificate
            #[derive(Debug)]
            struct NoVerifier;
            
            impl ServerCertVerifier for NoVerifier {
                fn verify_server_cert(
                    &self, _: &CertificateDer, _: &[CertificateDer], _: &ServerName, _: &[u8], _: UnixTime,
                ) -> Result<ServerCertVerified, RustlsError> {
                    Ok(ServerCertVerified::assertion())
                }
                
                fn verify_tls12_signature(
                    &self, _: &[u8], _: &CertificateDer, _: &DigitallySignedStruct,
                ) -> Result<HandshakeSignatureValid, RustlsError> {
                    Ok(HandshakeSignatureValid::assertion())
                }
                
                fn verify_tls13_signature(
                    &self, _: &[u8], _: &CertificateDer, _: &DigitallySignedStruct,
                ) -> Result<HandshakeSignatureValid, RustlsError> {
                    Ok(HandshakeSignatureValid::assertion())
                }
                
                fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                    vec![
                        SignatureScheme::RSA_PKCS1_SHA256, SignatureScheme::ECDSA_NISTP256_SHA256,
                        SignatureScheme::RSA_PSS_SHA256, SignatureScheme::ED25519,
                    ]
                }
            }
            
            let mut tls_config = ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerifier))
                .with_no_client_auth();
            tls_config.alpn_protocols = vec![b"h2".to_vec()];
            
            use tokio_rustls::TlsConnector;
            let connector = TlsConnector::from(Arc::new(tls_config));
            
            use hyper_util::rt::TokioIo;
            use tower::service_fn;
            
            let endpoint_owned = endpoint.to_string();
            let svc = service_fn(move |_uri: hyper::Uri| {
                let connector = connector.clone();
                let endpoint = endpoint_owned.clone();
                
                async move {
                    let addr = endpoint
                        .strip_prefix("https://").or_else(|| endpoint.strip_prefix("http://"))
                        .unwrap_or(&endpoint);
                    
                    let tcp = tokio::net::TcpStream::connect(addr).await
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    
                    let hostname = addr.split(':').next().unwrap_or(addr);
                    let server_name = ServerName::try_from(hostname)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    
                    let tls = connector.connect(server_name.to_owned(), tcp).await
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    
                    Ok::<_, std::io::Error>(TokioIo::new(tls))
                }
            });
            
            channel_builder.connect_with_connector(svc).await
                .map_err(|e| format!("connect error: {e}"))?
        } else if use_tls {
            // SECURE MODE: Use standard TLS validation with system root certificates
            use tonic::transport::ClientTlsConfig;
            
            let tls_config = ClientTlsConfig::new().with_enabled_roots();
            
            channel_builder
                .tls_config(tls_config)
                .map_err(|e| format!("tls config error: {e}"))?
                .connect()
                .await
                .map_err(|e| format!("connect error: {e}"))?
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
            end_of_stream: true,  // No body follows for headers-only exchange
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
