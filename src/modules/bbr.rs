use crate::model_extractor::extract_model_from_body;
use crate::modules::config::ModuleConfig;
use crate::Module;
use ngx::http::HttpModuleLocationConf;
use ngx::{core, http, ngx_log_debug_http};
use std::ffi::{c_char, c_void};

// Platform-conditional string pointer casting for nginx FFI
// macOS nginx FFI expects *const i8, Linux expects *const u8
#[cfg(target_os = "macos")]
#[inline]
fn cstr_ptr(s: *const u8) -> *const c_char {
    s as *const i8
}

#[cfg(not(target_os = "macos"))]
#[inline]
fn cstr_ptr(s: *const u8) -> *const c_char {
    s as *const c_char
}

/// Get an incoming request header value by name (case-insensitive).
pub fn get_header_in<'a>(request: &'a http::Request, key: &str) -> Option<&'a str> {
    for (name, value) in request.headers_in_iterator() {
        if let Ok(name_utf8) = name.to_str() {
            if name_utf8.eq_ignore_ascii_case(key) {
                if let Ok(val_utf8) = value.to_str() {
                    return Some(val_utf8);
                } else {
                    // Non-UTF8 value: skip (headers we need are ASCII)
                    return None;
                }
            }
        }
    }
    None
}

/// BBR (Body-Based Routing) processor
/// Extracts model information from JSON request bodies and sets appropriate headers
pub struct BbrProcessor;

impl BbrProcessor {
    /// Process BBR for a request if enabled
    pub fn process_request(request: &mut http::Request, conf: &ModuleConfig) -> core::Status {
        if !conf.bbr_enable {
            return core::Status::NGX_DECLINED;
        }

        ngx_log_debug_http!(
            request,
            "ngx-inference: BBR processing request, max_body_size: {}",
            conf.bbr_max_body_size
        );

        let header_name = if conf.bbr_header_name.is_empty() {
            "X-Gateway-Model-Name".to_string()
        } else {
            conf.bbr_header_name.clone()
        };

        // If header already present, skip BBR
        if get_header_in(request, &header_name).is_some() {
            ngx_log_debug_http!(
                request,
                "ngx-inference: BBR header {} already present, skipping",
                &header_name
            );
            return core::Status::NGX_DECLINED;
        }

        // Start body reading for BBR processing
        Self::start_body_reading(request, conf)
    }

    fn start_body_reading(request: &mut http::Request, conf: &ModuleConfig) -> core::Status {
        // Start reading the request body without pre-validation
        // We'll validate the actual body size during reading
        ngx_log_debug_http!(request, "ngx-inference: BBR starting body reading");

        let rc = unsafe {
            ngx::ffi::ngx_http_read_client_request_body(
                request.as_mut(),
                Some(bbr_body_read_handler),
            )
        };

        let status = if rc == isize::from(core::Status::NGX_OK) {
            core::Status::NGX_OK
        } else if rc == isize::from(core::Status::NGX_AGAIN) {
            core::Status::NGX_AGAIN
        } else {
            core::Status::NGX_ERROR
        };

        match status {
            core::Status::NGX_OK => core::Status::NGX_DONE, // Body reading complete, handler called
            core::Status::NGX_AGAIN => core::Status::NGX_DONE, // Body reading in progress, handler will be called
            _ => {
                if conf.bbr_failure_mode_allow {
                    core::Status::NGX_DECLINED
                } else {
                    core::Status::NGX_ERROR
                }
            }
        }
    }
}

/// Body read handler: called after ngx_http_read_client_request_body finishes reading.
///
/// # Safety
/// This function is called by nginx C code and must be marked unsafe because it:
/// - Dereferences raw pointers provided by nginx FFI
/// - Modifies nginx internal request structures
/// - Assumes the nginx request pointer is valid and not null
#[allow(clippy::manual_c_str_literals)] // FFI code uses byte strings for cross-platform compatibility
pub unsafe extern "C" fn bbr_body_read_handler(r: *mut ngx::ffi::ngx_http_request_t) {
    // Validate input pointer
    if r.is_null() {
        return;
    }

    // Check if request body processing is already complete or not available
    let request_body = (*r).request_body;
    if request_body.is_null() {
        // No request body structure, skip processing and continue
        return;
    }

    // Check if the body is still being read
    if (*request_body).rest > 0 {
        // Body is still being read, don't process yet
        return;
    }

    // Reconstruct Rust wrapper and config
    let request: &mut http::Request = ngx::http::Request::from_ngx_http_request(r);
    let conf = match Module::location_conf(request) {
        Some(c) => c,
        None => {
            // No config found, resume processing
            return;
        }
    };

    // Header name to set
    let header_name = if conf.bbr_header_name.is_empty() {
        "X-Gateway-Model-Name".to_string()
    } else {
        conf.bbr_header_name.clone()
    };

    // If header already present, skip BBR and resume.
    if get_header_in(request, &header_name).is_some() {
        return;
    }

    // Clear the request body post_handler to prevent re-execution
    (*(*r).request_body).post_handler = None;

    // Process the request body
    let body = match read_request_body(r, conf) {
        Ok(body) => body,
        Err(_) => {
            if conf.bbr_failure_mode_allow {
                // Continue with next phase instead of restarting phases
                return;
            } else {
                // Check if we already set a 413 status in read_request_body
                if (*r).headers_out.status
                    == ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_uint_t
                {
                    // 413 error - send special response and finalize so access logging runs
                    ngx::ffi::ngx_http_special_response_handler(
                        r,
                        ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_int_t,
                    );
                    ngx::ffi::ngx_http_finalize_request(
                        r,
                        ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_int_t,
                    );
                    return;
                } else {
                    // Other error - set 500, send special response and finalize so access logging runs
                    (*r).headers_out.status =
                        ngx::ffi::NGX_HTTP_INTERNAL_SERVER_ERROR as ngx::ffi::ngx_uint_t;
                    ngx::ffi::ngx_http_special_response_handler(
                        r,
                        ngx::ffi::NGX_HTTP_INTERNAL_SERVER_ERROR as ngx::ffi::ngx_int_t,
                    );
                    ngx::ffi::ngx_http_finalize_request(
                        r,
                        ngx::ffi::NGX_HTTP_INTERNAL_SERVER_ERROR as ngx::ffi::ngx_int_t,
                    );
                    return;
                }
            }
        }
    };

    // Extract model directly from JSON body
    if body.is_empty() {
        // Empty body - skip model extraction and continue processing
        return;
    }

    // Extract model name from JSON body and add header
    if let Some(model_name) = extract_model_from_body(&body) {
        // Add the model header to the request
        if request.add_header_in(&header_name, &model_name).is_some() {
            // Header set successfully - no logging needed for normal operation
        } else {
            ngx::ffi::ngx_log_error_core(
                ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                (*(*r).connection).log,
                0,
                cstr_ptr(b"ngx-inference: BBR failed to set header %*s: %*s\0".as_ptr()),
                header_name.len(),
                header_name.as_ptr(),
                model_name.len(),
                model_name.as_ptr(),
            );
        }
    } else {
        // No model found - use configured default to prevent reprocessing
        let default_model = &conf.bbr_default_model;
        let _ = request.add_header_in(&header_name, default_model);
        // Using default model is normal behavior - no logging needed
    }

    // Body processing complete - continue request phases
    // Mark body as processed and continue with next phases
    (*r).phase_handler += 1;
    ngx::ffi::ngx_http_core_run_phases(r);
}

/// Read the request body from memory and file buffers
unsafe fn read_request_body(
    r: *mut ngx::ffi::ngx_http_request_t,
    conf: &ModuleConfig,
) -> Result<Vec<u8>, ()> {
    let request_body = (*r).request_body;
    if request_body.is_null() {
        return Ok(Vec::new());
    }

    let bufs = (*request_body).bufs;
    if bufs.is_null() {
        return Ok(Vec::new());
    }

    // Get content length for pre-allocation hint (but don't trust it for validation)
    let content_length = {
        let request: &mut http::Request = ngx::http::Request::from_ngx_http_request(r);
        get_header_in(request, "Content-Length")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
    };

    // Cap memory allocation to reasonable size (1MB) to prevent excessive memory usage
    let safe_capacity = std::cmp::min(content_length, 1024 * 1024);
    let mut body: Vec<u8> = Vec::with_capacity(safe_capacity);
    let mut total_read = 0usize;

    let mut cl = bufs;
    while !cl.is_null() {
        let buf = (*cl).buf;
        if buf.is_null() {
            cl = (*cl).next;
            continue;
        }

        // Handle memory-backed buffers
        let pos = (*buf).pos;
        let last = (*buf).last;
        if !pos.is_null() && !last.is_null() && last >= pos {
            let len = last.offset_from(pos);
            if len > 0 {
                let len = len as usize;

                // Check if adding this buffer would exceed the BBR limit
                if total_read + len > conf.bbr_max_body_size {
                    let request: &mut http::Request = ngx::http::Request::from_ngx_http_request(r);
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: BBR actual body size {} exceeds limit {}",
                        total_read + len,
                        conf.bbr_max_body_size
                    );

                    if !conf.bbr_failure_mode_allow {
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                            (*(*r).connection).log,
                            0,
                            #[allow(clippy::manual_c_str_literals)] // FFI code
                            cstr_ptr(b"ngx-inference: BBR rejected request with HTTP 413 - actual payload size %uz exceeds limit %uz bytes\0".as_ptr()),
                            total_read + len,
                            conf.bbr_max_body_size
                        );
                        (*r).headers_out.status =
                            ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_uint_t;
                        return Err(());
                    } else {
                        // In allow mode, truncate to limit
                        let remaining = conf.bbr_max_body_size - total_read;
                        if remaining > 0 {
                            let slice = std::slice::from_raw_parts(pos as *const u8, remaining);
                            body.extend_from_slice(slice);
                        }
                        break;
                    }
                }

                let slice = std::slice::from_raw_parts(pos as *const u8, len);
                body.extend_from_slice(slice);
                total_read += len;
            }
        }

        // Handle file-backed buffers (for large bodies spilled to disk)
        let file = (*buf).file;
        if !file.is_null() {
            let file_pos = (*buf).file_pos;
            let file_last = (*buf).file_last;
            let file_size = (file_last - file_pos) as usize;

            if file_size > 0 {
                // Check if adding this file buffer would exceed the BBR limit
                if total_read + file_size > conf.bbr_max_body_size {
                    let request: &mut http::Request = ngx::http::Request::from_ngx_http_request(r);
                    ngx_log_debug_http!(
                        request,
                        "ngx-inference: BBR actual body size {} exceeds limit {}",
                        total_read + file_size,
                        conf.bbr_max_body_size
                    );

                    if !conf.bbr_failure_mode_allow {
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                            (*(*r).connection).log,
                            0,
                            #[allow(clippy::manual_c_str_literals)] // FFI code
                            cstr_ptr(b"ngx-inference: BBR rejected request with HTTP 413 - actual payload size %uz exceeds limit %uz bytes\0".as_ptr()),
                            total_read + file_size,
                            conf.bbr_max_body_size
                        );
                        (*r).headers_out.status =
                            ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_uint_t;
                        return Err(());
                    } else {
                        // In allow mode, read only what fits within the limit
                        let remaining = conf.bbr_max_body_size - total_read;
                        if remaining > 0 {
                            // Read partial file content up to the limit
                            let fd = (*file).fd;
                            if fd != -1 {
                                let mut file_buffer = vec![0u8; remaining];
                                let result = libc::pread(
                                    fd,
                                    file_buffer.as_mut_ptr() as *mut c_void,
                                    remaining,
                                    file_pos as libc::off_t,
                                );
                                if result > 0 {
                                    file_buffer.truncate(result as usize);
                                    body.extend_from_slice(&file_buffer);
                                }
                            }
                        }
                        break;
                    }
                }

                // Read from file descriptor
                let fd = (*file).fd;
                if fd != -1 {
                    // Create buffer for file content
                    let mut file_buffer = vec![0u8; file_size];
                    let mut bytes_read = 0usize;
                    // Read file content in chunks
                    while bytes_read < file_size {
                        let chunk_size = std::cmp::min(64 * 1024, file_size - bytes_read); // 64KB chunks
                        let result = libc::pread(
                            fd,
                            file_buffer.as_mut_ptr().add(bytes_read) as *mut c_void,
                            chunk_size,
                            (file_pos + bytes_read as i64) as libc::off_t,
                        );

                        if result <= 0 {
                            let request: &mut http::Request =
                                ngx::http::Request::from_ngx_http_request(r);
                            ngx_log_debug_http!(
                                request,
                                "ngx-inference: BBR file read error at offset {}",
                                bytes_read
                            );
                            break;
                        }
                        bytes_read += result as usize;
                    }

                    if bytes_read > 0 {
                        file_buffer.truncate(bytes_read);
                        body.extend_from_slice(&file_buffer);
                        total_read += bytes_read;
                        let request: &mut http::Request =
                            ngx::http::Request::from_ngx_http_request(r);
                        ngx_log_debug_http!(
                            request,
                            "ngx-inference: BBR read {} bytes from file, total: {}",
                            bytes_read,
                            total_read
                        );
                    }
                }
            }
        }

        cl = (*cl).next;
    }

    Ok(body)
}
