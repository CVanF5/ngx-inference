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

/// Watcher for timer-based result polling with eventfd notification
///
/// This structure is passed to the NGINX timer callback to check for
/// async EPP results. It contains a oneshot channel receiver, eventfd for
/// immediate notification, and the request pointer (only used in NGINX worker context).
///
/// Note: The timer event is allocated from the connection pool and will be
/// automatically freed when the connection closes.
pub struct ResultWatcher {
    /// Receiver for EPP result from async task
    pub receiver: oneshot::Receiver<Result<String, String>>,

    /// Raw request pointer - ONLY dereference in NGINX worker thread
    pub request: *mut ngx::ffi::ngx_http_request_t,

    /// Context for error handling
    pub ctx: AsyncEppContext,

    /// Start time in milliseconds (for timeout tracking)
    pub start_time_ms: u64,

    /// eventfd for immediate notification from Tokio thread
    pub eventfd: i32,
}

// Safety: ResultWatcher is Send because:
// 1. oneshot::Receiver is Send
// 2. The raw pointers are only dereferenced in the NGINX worker thread
// 3. NGINX event timers ensure the callback runs in the correct thread context
unsafe impl Send for ResultWatcher {}

impl ResultWatcher {
    /// Create a new result watcher with eventfd
    pub fn new(
        receiver: oneshot::Receiver<Result<String, String>>,
        request: *mut ngx::ffi::ngx_http_request_t,
        ctx: AsyncEppContext,
        eventfd: i32,
    ) -> Self {
        Self {
            receiver,
            request,
            ctx,
            start_time_ms: current_time_ms(),
            eventfd,
        }
    }

    /// Check if the timeout has been exceeded
    pub fn is_timed_out(&self) -> bool {
        let elapsed_ms = current_time_ms().saturating_sub(self.start_time_ms);
        elapsed_ms > self.ctx.timeout_ms
    }
}

impl Drop for ResultWatcher {
    fn drop(&mut self) {
        // Close eventfd when watcher is dropped
        if self.eventfd >= 0 {
            unsafe {
                libc::close(self.eventfd);
            }
        }
    }
}

/// Get current time in milliseconds
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

/// Create an eventfd for EPP result notification
///
/// Creates a non-blocking, close-on-exec eventfd for notifying NGINX
/// when async EPP tasks complete.
///
/// # Returns
///
/// - `Ok(fd)` with the eventfd file descriptor on success
/// - `Err(&str)` with error message on failure
pub fn create_eventfd() -> Result<i32, &'static str> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };

    if fd < 0 {
        Err("failed to create eventfd")
    } else {
        Ok(fd)
    }
}
