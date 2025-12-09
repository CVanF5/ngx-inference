//! Context structures for async EPP processing
//!
//! This module defines the data structures used to pass information between
//! NGINX worker thread and Tokio async tasks, ensuring thread safety.

use tokio::sync::oneshot;

/// Context for async EPP processing
///
/// This structure contains all the information needed to perform EPP processing
/// asynchronously, without requiring access to the NGINX request object.
#[derive(Debug, Clone)]
pub struct AsyncEppContext {
    /// EPP endpoint (e.g., "localhost:50051" or "https://epp.example.com")
    pub endpoint: String,

    /// Header name to set with upstream selection (e.g., "X-Inference-Upstream")
    pub upstream_header: String,

    /// Timeout in milliseconds for EPP call
    pub timeout_ms: u64,

    /// Request headers to send to EPP
    pub headers: Vec<(String, String)>,

    /// Whether to use TLS for gRPC connection
    pub use_tls: bool,

    /// Optional CA certificate file for TLS verification
    pub ca_file: Option<String>,

    /// Failure mode: true = fail-open, false = fail-closed
    pub failure_mode_allow: bool,

    /// Default upstream to use on EPP failure (if fail-open)
    pub default_upstream: Option<String>,
}

/// Watcher for timer-based result polling
///
/// This structure is passed to the NGINX timer callback to check for
/// async EPP results. It contains a oneshot channel receiver and the
/// request pointer (only used in NGINX worker context).
pub struct ResultWatcher {
    /// Receiver for EPP result from async task
    pub receiver: oneshot::Receiver<Result<String, String>>,

    /// Raw request pointer - ONLY dereference in NGINX worker thread
    pub request: *mut ngx::ffi::ngx_http_request_t,

    /// Context for error handling
    pub ctx: AsyncEppContext,
}

// Safety: ResultWatcher is Send because:
// 1. oneshot::Receiver is Send
// 2. The raw pointer is only dereferenced in the NGINX worker thread
// 3. NGINX event timers ensure the callback runs in the correct thread context
unsafe impl Send for ResultWatcher {}

impl ResultWatcher {
    /// Create a new result watcher
    pub fn new(
        receiver: oneshot::Receiver<Result<String, String>>,
        request: *mut ngx::ffi::ngx_http_request_t,
        ctx: AsyncEppContext,
    ) -> Self {
        Self {
            receiver,
            request,
            ctx,
        }
    }
}

/// Context for body read callback
///
/// This is passed to ngx_http_read_client_request_body and contains
/// the information needed to spawn the async EPP task after the body is read.
pub struct BodyReadContext {
    /// EPP configuration and parameters
    pub epp_ctx: AsyncEppContext,
}

impl BodyReadContext {
    /// Create a new body read context
    pub fn new(epp_ctx: AsyncEppContext) -> Self {
        Self { epp_ctx }
    }
}
