use crate::modules::{bbr::get_header_in, config::ModuleConfig};
use ngx::{core, http, ngx_log_debug_http};

// Helper macro for info-level logging in EPP
macro_rules! ngx_log_info_http {
    ($request:expr, $($arg:tt)*) => {{
        #[allow(unused_unsafe)]
        unsafe {
            let r = $request.as_mut();
            if let Some(conn) = r.connection.as_ref() {
                let msg = format!($($arg)*);
                if let Ok(c_msg) = std::ffi::CString::new(msg) {
                    ngx::ffi::ngx_log_error_core(
                        ngx::ffi::NGX_LOG_INFO as ngx::ffi::ngx_uint_t,
                        conn.log,
                        0,
                        c_msg.as_ptr(),
                    );
                }
            }
        }
    }};
}

// Helper macro for warning-level logging in EPP
macro_rules! ngx_log_warn_http {
    ($request:expr, $($arg:tt)*) => {{
        #[allow(unused_unsafe)]
        unsafe {
            let r = $request.as_mut();
            if let Some(conn) = r.connection.as_ref() {
                let msg = format!($($arg)*);
                if let Ok(c_msg) = std::ffi::CString::new(msg) {
                    ngx::ffi::ngx_log_error_core(
                        ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                        conn.log,
                        0,
                        c_msg.as_ptr(),
                    );
                }
            }
        }
    }};
}

/// Helper function to set default upstream header
fn set_default_upstream(
    request: &mut http::Request,
    conf: &ModuleConfig,
    upstream_header: &str,
    reason: &str,
) {
    if let Some(ref default_upstream) = conf.default_upstream {
        if get_header_in(request, upstream_header).is_none() {
            if request
                .add_header_in(upstream_header, default_upstream)
                .is_some()
            {
                ngx_log_warn_http!(
                    request,
                    "ngx-inference: Using default upstream '{}' due to {}",
                    default_upstream,
                    reason
                );
            } else {
                ngx_log_warn_http!(
                    request,
                    "ngx-inference: Failed to set default upstream header"
                );
            }
        }
    }
}

/// EPP (Endpoint Picker Processor) processor
/// Communicates with external gRPC services to determine upstream routing
pub struct EppProcessor;

impl EppProcessor {
    /// Process EPP for a request if enabled
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

        ngx_log_debug_http!(request, "ngx-inference: EPP starting upstream selection");

        // Use blocking approach - reliable and respects NGINX threading model
        match Self::pick_upstream_blocking(request, conf) {
            Ok(()) => {
                ngx_log_debug_http!(request, "ngx-inference: EPP upstream selection completed");
                core::Status::NGX_DECLINED
            }
            Err(err) => {
                ngx_log_warn_http!(
                    request,
                    "ngx-inference: EPP module failed to select upstream: {}",
                    err
                );
                if conf.epp_failure_mode_allow {
                    ngx_log_debug_http!(request, "ngx-inference: EPP continuing in fail-open mode");

                    let upstream_header = if conf.epp_header_name.is_empty() {
                        "X-Inference-Upstream"
                    } else {
                        &conf.epp_header_name
                    };
                    set_default_upstream(request, conf, upstream_header, "EPP failure");

                    core::Status::NGX_DECLINED
                } else {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: EPP returning error in fail-closed mode"
                    );
                    core::Status::NGX_ERROR
                }
            }
        }
    }

    /// Blocking upstream selection with comprehensive logging
    fn pick_upstream_blocking(
        request: &mut http::Request,
        conf: &ModuleConfig,
    ) -> Result<(), &'static str> {
        ngx_log_debug_http!(request, "ngx-inference: EPP pick_upstream_blocking started");

        // If EPP endpoint is not configured, skip.
        let endpoint = match &conf.epp_endpoint {
            Some(e) if !e.is_empty() => e.as_str(),
            _ => {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: EPP endpoint not configured, skipping"
                );
                return Ok(());
            }
        };

        let upstream_header = if conf.epp_header_name.is_empty() {
            "X-Inference-Upstream".to_string()
        } else {
            conf.epp_header_name.clone()
        };
        let upstream_header_str = upstream_header.as_str();

        // If upstream already set (e.g., previous stage), skip.
        if get_header_in(request, upstream_header_str).is_some() {
            ngx_log_debug_http!(
                request,
                "ngx-inference: Upstream header '{}' already set, skipping EPP",
                upstream_header_str
            );
            return Ok(());
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: EPP calling gRPC endpoint: {}",
            endpoint
        );

        // Collect headers for EPP
        let mut hdrs: Vec<(String, String)> = Vec::new();
        for (name, value) in request.headers_in_iterator() {
            if let (Ok(n), Ok(v)) = (name.to_str(), value.to_str()) {
                hdrs.push((n.to_string(), v.to_string()));
            }
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: EPP collected {} headers for processing",
            hdrs.len()
        );

        // Call gRPC EPP service
        match crate::grpc::epp_headers_blocking(
            request,
            endpoint,
            conf.epp_timeout_ms,
            upstream_header_str,
            hdrs,
            conf.epp_tls,
            conf.epp_ca_file.as_deref(),
        ) {
            Ok(Some(val)) => {
                ngx_log_info_http!(request, "ngx-inference: EPP selected upstream '{}'", val);
                // Write upstream selection header for variable consumption.
                if request.add_header_in(upstream_header_str, &val).is_some() {
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: EPP successfully set header '{}'",
                        upstream_header_str
                    );
                } else {
                    ngx_log_warn_http!(
                        request,
                        "ngx-inference: EPP failed to set header '{}'",
                        upstream_header_str
                    );
                    return Err("failed to set upstream header");
                }
            }
            Ok(None) => {
                ngx_log_debug_http!(
                    request,
                    "ngx-inference: EPP gRPC success: No upstream provided by EPP server"
                );

                // No upstream provided by EPP
                if conf.epp_failure_mode_allow {
                    // Fail-open mode: try to use default upstream
                    set_default_upstream(
                        request,
                        conf,
                        upstream_header_str,
                        "EPP returned no upstream",
                    );
                } else {
                    // Fail-closed mode: EPP must provide an upstream, otherwise fail
                    return Err("EPP returned no upstream in fail-closed mode");
                }
            }
            Err(err) => {
                ngx_log_warn_http!(request, "ngx-inference: EPP gRPC error: {}", err);
                return Err("epp grpc error");
            }
        }

        ngx_log_debug_http!(request, "ngx-inference: EPP processing completed");
        Ok(())
    }
}
