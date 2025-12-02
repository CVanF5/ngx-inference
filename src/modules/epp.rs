use crate::modules::{bbr::get_header_in, config::ModuleConfig};
use ngx::{core, http};

// Helper macro for info-level logging in EPP
macro_rules! ngx_log_info_http {
    ($request:expr, $($arg:tt)*) => {
        unsafe {
            let msg = format!($($arg)*);
            let c_msg = std::ffi::CString::new(msg).unwrap();
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_INFO as ngx::ffi::ngx_uint_t,
                ($request.as_mut().connection.as_ref().unwrap().log),
                0,
                c_msg.as_ptr(),
            );
        }
    };
}

// Helper macro for debug-level logging in EPP
macro_rules! ngx_log_debug_http {
    ($request:expr, $($arg:tt)*) => {
        unsafe {
            let msg = format!($($arg)*);
            let c_msg = std::ffi::CString::new(msg).unwrap();
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_DEBUG_HTTP as ngx::ffi::ngx_uint_t,
                ($request.as_mut().connection.as_ref().unwrap().log),
                0,
                c_msg.as_ptr(),
            );
        }
    };
}

/// EPP (Endpoint Picker Processor) processor
/// Communicates with external gRPC services to determine upstream routing
pub struct EppProcessor;

impl EppProcessor {
    /// Process EPP for a request if enabled
    pub fn process_request(request: &mut http::Request, conf: &ModuleConfig) -> core::Status {
        if !conf.epp_enable {
            return core::Status::NGX_DECLINED;
        }

        // Use blocking approach - reliable and respects NGINX threading model
        match Self::pick_upstream_blocking(request, conf) {
            Ok(()) => core::Status::NGX_OK,
            Err(err) => {
                if conf.epp_failure_mode_allow {
                    ngx_log_info_http!(
                        request,
                        "ngx-inference: EPP failed ({}), continuing in fail-open mode",
                        err
                    );
                    core::Status::NGX_OK
                } else {
                    ngx_log_info_http!(
                        request,
                        "ngx-inference: EPP failed ({}), returning error in fail-closed mode",
                        err
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
            "ngx-inference: Starting EPP blocking operation to endpoint: {}",
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
                        "ngx-inference: EPP successfully set header '{}' = '{}'",
                        upstream_header_str,
                        val
                    );
                } else {
                    ngx_log_info_http!(
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
                // No upstream provided - this is valid behavior
            }
            Err(err) => {
                ngx_log_info_http!(request, "ngx-inference: EPP gRPC error: {}", err);
                return Err("epp grpc error");
            }
        }

        ngx_log_debug_http!(request, "ngx-inference: EPP processing completed");
        Ok(())
    }
}
