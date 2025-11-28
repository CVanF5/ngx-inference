//! Standard BBR (Body-Based Routing) and EPP (Endpoint Picker Processor) mock server.
//!
//! This implements the Gateway API Inference Extension protocols:
//! - BBR: Streams request body, detects model from JSON, returns X-Gateway-Model-Name header
//! - EPP: Headers-only exchange, returns X-Inference-Upstream header for endpoint selection
//!
//! BBR Mode (port 9000):
//! - On RequestHeaders: waits for body streaming
//! - On RequestBody chunks: accumulates until EndOfStream
//! - Parses JSON for "model" field, extracts value for X-Gateway-Model-Name header
//!
//! EPP Mode (port 9001):
//! - On RequestHeaders: immediately responds with X-Inference-Upstream header
//!
//! Configuration via environment variables:
//! - EPP_UPSTREAM: value for X-Inference-Upstream (default: "host.docker.internal:18080")
//! - BBR_MODEL: fallback model name if not found in JSON (default: "bbr-chosen-model")
//!
//! CLI:
//!   cargo run --bin extproc_mock -- 0.0.0.0:9001  # EPP mode
//!   cargo run --bin extproc_mock -- 0.0.0.0:9000  # BBR mode

use std::{env, net::SocketAddr};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

mod protos {
    // Reuse the shared proto module in this bin without linking to the NGINX lib,
    // avoiding unresolved NGINX symbols at link time.
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/protos.rs"));
}
use crate::protos::envoy;

type ProcessingRequest = envoy::service::ext_proc::v3::ProcessingRequest;
type ProcessingResponse = envoy::service::ext_proc::v3::ProcessingResponse;

type HeadersResponse = envoy::service::ext_proc::v3::HeadersResponse;
type BodyResponse = envoy::service::ext_proc::v3::BodyResponse;
type CommonResponse = envoy::service::ext_proc::v3::common_response::ResponseStatus;
type HeaderMutation = envoy::service::ext_proc::v3::HeaderMutation;

type HeaderValue = envoy::config::core::v3::HeaderValue;
type HeaderValueOption = envoy::config::core::v3::HeaderValueOption;

use envoy::service::ext_proc::v3::external_processor_server::{
    ExternalProcessor, ExternalProcessorServer,
};
use envoy::service::ext_proc::v3::processing_request;
use envoy::service::ext_proc::v3::processing_response;
use serde_json::Value;

fn hv(key: &str, value: &str) -> HeaderValue {
    HeaderValue {
        key: key.to_string(),
        value: value.to_string(),
        raw_value: Vec::new(),
    }
}

fn hvo(key: &str, value: &str) -> HeaderValueOption {
    HeaderValueOption {
        header: Some(hv(key, value)),
        ..Default::default()
    }
}

fn build_header_mutation_headers(epp_upstream: &str) -> HeaderMutation {
    HeaderMutation {
        set_headers: vec![hvo("X-Inference-Upstream", epp_upstream)],
        remove_headers: Vec::new(),
    }
}

fn build_header_mutation_bbr(bbr_model: &str) -> HeaderMutation {
    HeaderMutation {
        set_headers: vec![hvo("X-Gateway-Model-Name", bbr_model)],
        remove_headers: Vec::new(),
    }
}

fn build_headers_response(epp_upstream: &str, _bbr_model: &str) -> HeadersResponse {
    let mutation = build_header_mutation_headers(epp_upstream);
    envoy::service::ext_proc::v3::HeadersResponse {
        response: Some(envoy::service::ext_proc::v3::CommonResponse {
            status: CommonResponse::Continue as i32,
            header_mutation: Some(mutation),
            body_mutation: None,
            trailers: None,
            clear_route_cache: false,
        }),
    }
}

fn build_body_response(_epp_upstream: &str, bbr_model: &str) -> BodyResponse {
    let mutation = build_header_mutation_bbr(bbr_model);
    envoy::service::ext_proc::v3::BodyResponse {
        response: Some(envoy::service::ext_proc::v3::CommonResponse {
            status: CommonResponse::Continue as i32,
            header_mutation: Some(mutation),
            body_mutation: None,
            trailers: None,
            clear_route_cache: false,
        }),
    }
}

#[derive(Clone)]
struct ExtProcMock {
    epp_upstream: String,
    bbr_model: String,
    role: String,
}

#[tonic::async_trait]
impl ExternalProcessor for ExtProcMock {
    type ProcessStream = ReceiverStream<Result<ProcessingResponse, Status>>;
    async fn process(
        &self,
        request: Request<tonic::Streaming<ProcessingRequest>>,
    ) -> Result<Response<Self::ProcessStream>, Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<ProcessingResponse, Status>>(32);
        let epp_upstream = self.epp_upstream.clone();
        let bbr_model = self.bbr_model.clone();
        let role = self.role.clone();
        tokio::spawn(async move {
            let mut sent_headers_response = false;
            let mut body_buf: Vec<u8> = Vec::new();
            let mut current_bbr_model = bbr_model.clone();
            while let Some(msg) = inbound.message().await.transpose() {
                match msg {
                    Ok(pr) => match pr.request {
                        Some(processing_request::Request::RequestHeaders(_)) => {
                            if role == "EPP" {
                                eprintln!(
                                    "extproc_mock: EPP headers received, selecting endpoint: {}",
                                    epp_upstream
                                );
                                let resp = ProcessingResponse {
                                    response: Some(processing_response::Response::RequestHeaders(
                                        build_headers_response(&epp_upstream, &bbr_model),
                                    )),
                                    dynamic_metadata: None,
                                    mode_override: None,
                                    override_message_timeout: None,
                                };
                                if tx.send(Ok(resp)).await.is_err() {
                                    break;
                                }
                                sent_headers_response = true;
                            } else {
                                eprintln!(
                                    "extproc_mock: BBR headers received, waiting for body..."
                                );
                            }
                        }
                        Some(processing_request::Request::RequestBody(body)) => {
                            body_buf.extend_from_slice(&body.body);
                            if body.end_of_stream {
                                eprintln!(
                                    "extproc_mock: end of stream, body size: {} bytes",
                                    body_buf.len()
                                );
                                if let Ok(v) = serde_json::from_slice::<Value>(&body_buf) {
                                    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                                        current_bbr_model = m.to_string();
                                        eprintln!(
                                            "extproc_mock: detected model in JSON body: {}",
                                            current_bbr_model
                                        );
                                    }
                                }
                                let resp = ProcessingResponse {
                                    response: Some(processing_response::Response::RequestBody(
                                        build_body_response(&epp_upstream, &current_bbr_model),
                                    )),
                                    dynamic_metadata: None,
                                    mode_override: None,
                                    override_message_timeout: None,
                                };
                                if role == "BBR" {
                                    eprintln!(
                                        "extproc_mock: BBR final response - model: {}",
                                        current_bbr_model
                                    );
                                }
                                if tx.send(Ok(resp)).await.is_err() {
                                    break;
                                }
                            } else {
                                eprintln!("extproc_mock: received body chunk, size: {} bytes, total: {} bytes", body.body.len(), body_buf.len());
                            }
                        }
                        _ => {}
                    },
                    Err(status) => {
                        let _ = tx.send(Err(status)).await;
                        break;
                    }
                }
            }
            if !sent_headers_response && role == "EPP" {
                let resp = ProcessingResponse {
                    response: Some(processing_response::Response::RequestHeaders(
                        build_headers_response(&epp_upstream, &bbr_model),
                    )),
                    dynamic_metadata: None,
                    mode_override: None,
                    override_message_timeout: None,
                };
                let _ = tx.send(Ok(resp)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:9001".to_string())
        .parse()?;
    let epp_upstream =
        env::var("EPP_UPSTREAM").unwrap_or_else(|_| "host.docker.internal:18080".to_string());
    let bbr_model = env::var("BBR_MODEL").unwrap_or_else(|_| "bbr-chosen-model".to_string());
    let default_role = if addr.port() == 9001 {
        "EPP"
    } else if addr.port() == 9000 {
        "BBR"
    } else {
        "EPP"
    };
    let role = env::var("MOCK_ROLE").unwrap_or_else(|_| default_role.to_string());

    println!(
        "extproc_mock: role={}, configured EPP_UPSTREAM={}, BBR_MODEL={}",
        role, epp_upstream, bbr_model
    );

    let svc = ExtProcMock {
        epp_upstream,
        bbr_model,
        role,
    };

    println!("extproc_mock listening on {}", addr);
    tonic::transport::Server::builder()
        .add_service(ExternalProcessorServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}
