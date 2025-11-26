//! Minimal Envoy ExternalProcessor (ext-proc) mock server for testing ngx-inference.
//!
//! Behavior:
//! - On receiving RequestHeaders: immediately respond with a HeadersResponse that can include:
//!     * X-Inference-Upstream    (EPP) -> default "host.docker.internal:18080" (overridable)
//!     * X-Gateway-Model-Name    (BBR) -> default "bbr-chosen-model"          (overridable)
//! - On receiving RequestBody chunks (STREAMED mode): it also responds with a BodyResponse
//!   containing the same header mutation (helpful for BBR flows that expect responses while streaming).
//!
//! Configuration via environment variables (optional):
//! - EPP_UPSTREAM: value for X-Inference-Upstream (default: "host.docker.internal:18080")
//! - BBR_MODEL:    value for X-Gateway-Model-Name (default: "bbr-chosen-model")
//!
//! CLI:
//!   cargo run --bin extproc_mock -- 0.0.0.0:9001
//!   cargo run --bin extproc_mock -- 0.0.0.0:9000
//!
//! Notes:
//! - This mock sets both headers on both header/body responses. The ngx-inference module will
//!   pick whichever header it is configured to look for (EPP/BBR). Setting both keeps the mock simple.
//! - For end-to-end proxying, ensure nginx.conf has a working resolver (e.g. 127.0.0.11 in Docker)
//!   and that the upstream you set is reachable (e.g. run: python3 -m http.server 18080).

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
        // The generated API for HeaderValueOption includes fields like append_action and
        // maybe keep_empty. We rely on protobuf defaults here (replace behavior).
        // If your generated bindings differ, set append_action accordingly.
        ..Default::default()
    }
}

fn build_header_mutation_headers(epp_upstream: &str) -> HeaderMutation {
    HeaderMutation {
        set_headers: vec![
            hvo("X-Inference-Upstream", epp_upstream),
        ],
        remove_headers: Vec::new(),
    }
}

fn build_header_mutation_body(epp_upstream: &str, bbr_model: &str) -> HeaderMutation {
    HeaderMutation {
        set_headers: vec![
            hvo("X-Gateway-Model-Name", bbr_model),
            hvo("X-Inference-Upstream", epp_upstream),
        ],
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

fn build_body_response(epp_upstream: &str, bbr_model: &str) -> BodyResponse {
    let mutation = build_header_mutation_body(epp_upstream, bbr_model);
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

        // Outbound channel/stream
        let (tx, rx) = mpsc::channel::<Result<ProcessingResponse, Status>>(32);

        let epp_upstream = self.epp_upstream.clone();
        let bbr_model = self.bbr_model.clone();
        let role = self.role.clone();

        // Spawn a task to read inbound messages and respond
        tokio::spawn(async move {
            let mut sent_headers_response = false;
            let mut body_buf: Vec<u8> = Vec::new();
            let mut current_bbr_model = bbr_model.clone();

            while let Some(msg) = inbound.message().await.transpose() {
                match msg {
                    Ok(pr) => {
                        match pr.request {
                            Some(processing_request::Request::RequestHeaders(_hdrs)) => {
                                // On headers: send a HeadersResponse with header_mutation
                                if role == "EPP" {
                                    eprintln!("extproc_mock: mock selected endpoint (EPP): {}", epp_upstream);
                                }
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
                            }
                            Some(processing_request::Request::RequestBody(body)) => {
                                // Accumulate request body and attempt to parse OpenAI-style JSON to extract "model"
                                // Example: {"model":"gpt-4o-mini","input":"Hello", ...}
                                body_buf.extend_from_slice(&body.body);
                                if let Ok(v) = serde_json::from_slice::<Value>(&body_buf) {
                                    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                                        current_bbr_model = m.to_string();
                                        eprintln!("extproc_mock: detected model in body: {}", current_bbr_model);
                                    }
                                }

                                // Send a BodyResponse that carries header mutation with the (possibly updated) model
                                if role == "EPP" {
                                    eprintln!(
                                        "extproc_mock: streaming - mock selected endpoint (EPP): {}, model: {}",
                                        epp_upstream,
                                        current_bbr_model
                                    );
                                } else {
                                    eprintln!("extproc_mock: streaming - BBR model: {}", current_bbr_model);
                                }
                                let resp = ProcessingResponse {
                                    response: Some(processing_response::Response::RequestBody(
                                        build_body_response(&epp_upstream, &current_bbr_model),
                                    )),
                                    dynamic_metadata: None,
                                    mode_override: None,
                                    override_message_timeout: None,
                                };
                                if tx.send(Ok(resp)).await.is_err() {
                                    break;
                                }
                            }
                            Some(processing_request::Request::RequestTrailers(_)) => {
                                // No-op for this mock
                            }
                            Some(processing_request::Request::ResponseHeaders(_)) => {
                                // Not used in request path; ignore
                            }
                            Some(processing_request::Request::ResponseBody(_)) => {
                                // Not used in request path; ignore
                            }
                            Some(processing_request::Request::ResponseTrailers(_)) => {
                                // Not used
                            }
                            None => {
                                // Unexpected empty message; ignore
                            }
                        }
                    }
                    Err(status) => {
                        let _ = tx.send(Err(status)).await;
                        break;
                    }
                }
            }

            // In case no headers/body were ever seen, we could still emit a headers response.
            if !sent_headers_response {
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
    // Bind address from CLI or default to 0.0.0.0:9001
    let addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:9001".to_string())
        .parse()?;

    // Configuration (can override with env)
    let epp_upstream = env::var("EPP_UPSTREAM").unwrap_or_else(|_| "host.docker.internal:18080".to_string());
    let bbr_model = env::var("BBR_MODEL").unwrap_or_else(|_| "bbr-chosen-model".to_string());
    let default_role = if addr.port() == 9001 { "EPP" } else if addr.port() == 9000 { "BBR" } else { "EPP" };
    let role = env::var("MOCK_ROLE").unwrap_or_else(|_| default_role.to_string());

    println!("extproc_mock: role={}, configured EPP_UPSTREAM={}, BBR_MODEL={}", role, epp_upstream, bbr_model);

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
