//! Async EPP processor running on Tokio thread pool
//!
//! This module implements the actual EPP processing logic that runs asynchronously
//! on the Tokio runtime. It must NOT call any NGINX FFI functions.

use crate::epp::context::AsyncEppContext;
use crate::grpc::epp_headers_blocking_internal;
use std::sync::OnceLock;
use tokio::sync::oneshot;

/// Global Tokio runtime for async EPP processing
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Get or create the global Tokio runtime
pub fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("epp-worker")
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime for EPP")
    })
}

/// Spawn an async EPP task
///
/// This function spawns a Tokio task that performs the EPP gRPC call asynchronously.
/// The result is sent back through the oneshot channel and eventfd is notified.
///
/// # Thread Safety
///
/// This function is called from the NGINX worker thread but spawns a task on the
/// Tokio thread pool. The spawned task MUST NOT call any NGINX FFI functions.
///
/// # Parameters
///
/// - `ctx`: EPP configuration and request context
/// - `body`: Request body bytes
/// - `sender`: Oneshot channel to send the result
/// - `eventfd`: File descriptor to notify when result is ready
pub fn spawn_epp_task(
    ctx: AsyncEppContext,
    body: Vec<u8>,
    sender: oneshot::Sender<Result<String, String>>,
    eventfd: i32,
) {
    let rt = get_runtime();

    rt.spawn(async move {
        let result = process_epp_async(ctx, body).await;

        // Send result back to NGINX worker thread via channel
        // Ignore send errors (channel dropped means request was cancelled)
        let _ = sender.send(result);

        // Notify NGINX via eventfd (write any non-zero value)
        // This triggers immediate notification instead of waiting for timer
        let value: u64 = 1;
        unsafe {
            libc::write(
                eventfd,
                &value as *const u64 as *const libc::c_void,
                std::mem::size_of::<u64>(),
            );
        }
        // Note: We don't close eventfd here - ResultWatcher Drop handles that
    });
}

/// Process EPP request asynchronously
///
/// This function performs the actual EPP gRPC call. It runs on a Tokio worker thread
/// and must NOT call any NGINX FFI functions.
///
/// # Parameters
///
/// - `ctx`: EPP configuration and request context
/// - `body`: Request body bytes (for future body-aware EPP processing)
///
/// # Returns
///
/// - `Ok(upstream_name)` if EPP successfully selected an upstream
/// - `Err(error_message)` if EPP failed
async fn process_epp_async(ctx: AsyncEppContext, _body: Vec<u8>) -> Result<String, String> {
    // For now, we're doing headers-only EPP (like the current implementation)
    // The body parameter is included for future extension to body-aware EPP

    let endpoint = &ctx.endpoint;
    let timeout_ms = ctx.timeout_ms;
    let header_name = &ctx.upstream_header;
    let headers = ctx.headers.clone();
    let use_tls = ctx.use_tls;
    let ca_file = ctx.ca_file.as_deref();

    // Call the internal async EPP function
    // This function doesn't use any NGINX logging, making it safe for async context
    match epp_headers_blocking_internal(
        endpoint,
        timeout_ms,
        header_name,
        headers,
        use_tls,
        ca_file,
    )
    .await
    {
        Ok(Some(upstream)) => {
            // EPP returned an upstream selection
            Ok(upstream)
        }
        Ok(None) => {
            // EPP didn't return an upstream
            // The caller will handle this based on failure_mode_allow
            Err("EPP returned no upstream".to_string())
        }
        Err(e) => {
            // gRPC or network error
            Err(format!("EPP error: {}", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let rt = get_runtime();
        assert!(rt.handle().metrics().num_workers() > 0);
    }

    #[tokio::test]
    async fn test_process_epp_async_no_endpoint() {
        let ctx = AsyncEppContext {
            endpoint: "".to_string(),
            upstream_header: "X-Inference-Upstream".to_string(),
            timeout_ms: 100,
            headers: vec![],
            use_tls: false,
            ca_file: None,
            failure_mode_allow: true,
            default_upstream: None,
        };

        let result = process_epp_async(ctx, vec![]).await;
        assert!(result.is_err());
    }
}
