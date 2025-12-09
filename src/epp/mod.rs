//! Non-blocking EPP (Endpoint Picker Processor) implementation
//!
//! This module implements a non-blocking EPP processor that allows NGINX workers to remain
//! responsive while EPP processing happens asynchronously on a separate Tokio thread pool.
//!
//! # Architecture
//!
//! ```text
//! 1. NGINX Worker receives request
//!    ↓
//! 2. Read request body using ngx_http_read_client_request_body (non-blocking for other requests)
//!    ↓
//! 3. In body_read_callback: Extract body, spawn Tokio task with oneshot channel
//!    ↓
//! 4. Return control to NGINX worker (now free to handle other requests)
//!    ↓
//! 5. Tokio thread pool handles gRPC EPP call asynchronously
//!    ↓
//! 6. NGINX timer polls oneshot channel every 1ms (runs in worker context)
//!    ↓
//! 7. When result ready: Set upstream header, finalize request
//! ```
//!
//! # Thread Safety
//!
//! - All NGINX API calls happen only in the worker thread context
//! - Tokio tasks run on separate threads and never call NGINX APIs
//! - Communication happens via thread-safe oneshot channels
//! - Raw pointers are only dereferenced in the correct thread context

pub mod async_processor;
pub mod callbacks;
pub mod context;

use crate::modules::config::ModuleConfig;
use ngx::{core, http, ngx_log_debug_http};

// Re-export for convenience
pub use context::AsyncEppContext;

/// EPP Processor with non-blocking async support
pub struct EppProcessor;

impl EppProcessor {
    /// Process EPP for a request if enabled
    ///
    /// This initiates non-blocking EPP processing by reading the request body
    /// and spawning an async task. Returns NGX_DONE if async processing started,
    /// NGX_DECLINED if EPP is disabled or skipped, or NGX_ERROR on failure.
    pub fn process_request(request: &mut http::Request, conf: &ModuleConfig) -> core::Status {
        ngx_log_debug_http!(
            request,
            "ngx-inference: EPP process_request called, enabled={}",
            conf.epp_enable
        );

        if !conf.epp_enable {
            ngx_log_debug_http!(request, "ngx-inference: EPP disabled, declining");
            return core::Status::NGX_DECLINED;
        }

        // Check if EPP endpoint is configured
        let endpoint = match &conf.epp_endpoint {
            Some(e) if !e.is_empty() => e.as_str(),
            _ => {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: EPP endpoint not configured, skipping"
                );
                return core::Status::NGX_DECLINED;
            }
        };

        let upstream_header = if conf.epp_header_name.is_empty() {
            "X-Inference-Upstream"
        } else {
            &conf.epp_header_name
        };

        // If upstream already set, skip EPP
        if crate::modules::bbr::get_header_in(request, upstream_header).is_some() {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Upstream header '{}' already set, skipping EPP",
                upstream_header
            );
            return core::Status::NGX_DECLINED;
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: Starting non-blocking EPP processing for endpoint: {}",
            endpoint
        );

        // Collect headers before async processing
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, value) in request.headers_in_iterator() {
            if let (Ok(n), Ok(v)) = (name.to_str(), value.to_str()) {
                headers.push((n.to_string(), v.to_string()));
            }
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: Collected {} headers for EPP processing",
            headers.len()
        );

        // Create context for async processing
        let ctx = AsyncEppContext {
            endpoint: endpoint.to_string(),
            upstream_header: upstream_header.to_string(),
            timeout_ms: conf.epp_timeout_ms,
            headers,
            use_tls: conf.epp_tls,
            ca_file: conf.epp_ca_file.clone(),
            failure_mode_allow: conf.epp_failure_mode_allow,
            default_upstream: conf.default_upstream.clone(),
        };

        // Check if body has already been read (e.g., by BBR)
        let r = request.as_mut();

        // Check if request has already been finalized with an error (e.g., BBR 413)
        let status = r.headers_out.status;

        if status >= 300 {
            ngx_log_debug_http!(
                request,
                "ngx-inference: EPP skipping - request already has error status {}",
                status
            );
            return core::Status::NGX_DECLINED;
        }

        let request_body = r.request_body;

        if !request_body.is_null() {
            // Body read has been initiated (by BBR or previous handler)
            let rest = unsafe { (*request_body).rest };
            if rest == 0 {
                // Body is complete, use it directly
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: EPP using pre-read body (already read by BBR or earlier handler)"
                );
                return callbacks::process_with_existing_body(request, ctx);
            } else {
                // Body is still being read by another handler (BBR)
                // CRITICAL: Do NOT call ngx_http_read_client_request_body again!
                // It would overwrite the existing callback and cause crashes
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: EPP declining - body already being read by another handler (rest={})",
                    rest
                );
                return core::Status::NGX_DECLINED;
            }
        }

        // Body hasn't been read yet, initiate non-blocking body read
        // The callback will handle spawning the async task
        callbacks::read_body_async(request, ctx)
    }
}
