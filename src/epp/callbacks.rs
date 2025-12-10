//! NGINX callbacks for non-blocking EPP processing
//!
//! This module implements the C callbacks that interface with NGINX's event loop.
//! All functions in this module run in the NGINX worker thread context.

use crate::epp::async_processor;
use crate::epp::context::{AsyncEppContext, ResultWatcher};
use ngx::core;
use ngx::ffi::{
    ngx_add_timer, ngx_del_timer, ngx_event_t, ngx_http_core_run_phases, ngx_http_finalize_request,
    ngx_http_read_client_request_body, ngx_http_request_t, ngx_int_t, ngx_msec_t,
};
use ngx::http::HttpModuleLocationConf;
use std::ffi::{c_char, c_void, CString};
use tokio::sync::oneshot;

/// Timer poll interval in milliseconds
const TIMER_INTERVAL_MS: ngx_msec_t = 1;

/// Chunk size for reading file-backed request bodies
const FILE_READ_CHUNK_SIZE: usize = 64 * 1024; // 64 KB
/// Invalid file descriptor constant
const INVALID_FD: i32 = -1;

// Platform-agnostic string pointer casting for nginx FFI
// c_char can be either i8 or u8 depending on platform
#[inline]
fn cstr_ptr(s: *const u8) -> *const c_char {
    s.cast::<c_char>()
}

/// Helper macro for error logging from raw request pointer
macro_rules! ngx_log_error_raw {
    ($request:expr, $($arg:tt)*) => {{
        let r = $request;
        if !r.is_null() {
            unsafe {
                let r_ref = &*r;
                if let Some(conn) = r_ref.connection.as_ref() {
                    let msg = format!($($arg)*);
                    if let Ok(c_msg) = std::ffi::CString::new(msg) {
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                            conn.log,
                            0,
                            c_msg.as_ptr(),
                        );
                    }
                }
            }
        }
    }};
}

/// Helper macro for debug logging from raw request pointer
macro_rules! ngx_log_debug_raw {
    ($request:expr, $($arg:tt)*) => {{
        let r = $request;
        if !r.is_null() {
            unsafe {
                let r_ref = &*r;
                if let Some(conn) = r_ref.connection.as_ref() {
                    let msg = format!($($arg)*);
                    if let Ok(c_msg) = std::ffi::CString::new(msg) {
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_DEBUG as ngx::ffi::ngx_uint_t,
                            conn.log,
                            0,
                            c_msg.as_ptr(),
                        );
                    }
                }
            }
        }
    }};
}

/// Helper macro for info logging from raw request pointer
macro_rules! ngx_log_info_raw {
    ($request:expr, $($arg:tt)*) => {{
        let r = $request;
        if !r.is_null() {
            unsafe {
                let r_ref = &*r;
                if let Some(conn) = r_ref.connection.as_ref() {
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
        }
    }};
}

/// Process EPP with body that has already been read (e.g., by BBR)
///
/// This function extracts the already-read body and processes it immediately,
/// bypassing the need to call ngx_http_read_client_request_body again.
///
/// # Thread Safety
///
/// This function runs in the NGINX worker thread and is safe to call.
pub fn process_with_existing_body(
    request: &mut ngx::http::Request,
    ctx: AsyncEppContext,
) -> core::Status {
    let r: *mut ngx_http_request_t = request.as_mut();

    ngx_log_debug_raw!(r, "ngx-inference: EPP processing with existing body");

    // Extract the already-read body
    let body = match unsafe { extract_request_body(r) } {
        Ok(b) => b,
        Err(e) => {
            ngx_log_error_raw!(
                r,
                "ngx-inference: EPP failed to extract pre-read body: {}",
                e
            );
            if ctx.failure_mode_allow {
                return core::Status::NGX_DECLINED;
            } else {
                return core::Status::NGX_ERROR;
            }
        }
    };

    ngx_log_debug_raw!(
        r,
        "ngx-inference: EPP extracted {} bytes from pre-read body",
        body.len()
    );

    // Create oneshot channel for result
    let (sender, receiver) = oneshot::channel();

    // Spawn async EPP task
    async_processor::spawn_epp_task(ctx.clone(), body, sender);

    ngx_log_debug_raw!(r, "ngx-inference: EPP async task spawned, setting up timer");

    // Create result watcher
    let watcher = Box::new(ResultWatcher::new(receiver, r, ctx));
    let watcher_ptr = Box::into_raw(watcher);

    // Set up timer to poll for results
    if !unsafe { setup_result_timer(r, watcher_ptr) } {
        ngx_log_error_raw!(r, "ngx-inference: EPP failed to setup result timer");
        unsafe {
            let _ = Box::from_raw(watcher_ptr);
        }
        return core::Status::NGX_ERROR;
    }

    // Return NGX_DONE to suspend request until async processing completes
    core::Status::NGX_DONE
}

/// Initiate non-blocking body read for EPP processing
///
/// This function starts the async EPP processing pipeline by requesting
/// the request body. NGINX will call our callback when the body is available.
///
/// # Thread Safety
///
/// This function runs in the NGINX worker thread and is safe to call.
pub fn read_body_async(request: &mut ngx::http::Request, _ctx: AsyncEppContext) -> core::Status {
    let r: *mut ngx_http_request_t = request.as_mut();

    ngx_log_debug_raw!(r, "ngx-inference: EPP initiating body read");

    // DON'T use (*r).ctx - it causes free() errors
    // Instead, we'll reconstruct context from request config in the callback

    // Request body read with callback
    // NGINX will call body_read_done when body is available
    let rc = unsafe { ngx_http_read_client_request_body(r, Some(body_read_done)) };

    if rc >= 300 as ngx_int_t {
        // Body read failed with HTTP error
        ngx_log_error_raw!(r, "ngx-inference: EPP body read failed with error: {}", rc);
        return core::Status::NGX_ERROR;
    }

    if rc == core::Status::NGX_AGAIN.0 as ngx_int_t {
        // Body read is async - set write_event_handler and finalize with NGX_DONE
        // This is CRITICAL for proper async handling (copied from BBR pattern)
        ngx_log_debug_raw!(r, "ngx-inference: EPP body read in progress (async)");
        unsafe {
            (*r).write_event_handler = Some(ngx::ffi::ngx_http_core_run_phases);
            ngx_http_finalize_request(r, core::Status::NGX_DONE.0 as ngx_int_t);
        }
        core::Status::NGX_DONE
    } else {
        // Body was already buffered, callback already called - still need finalize
        ngx_log_debug_raw!(r, "ngx-inference: EPP body read completed (sync)");
        unsafe {
            ngx_http_finalize_request(r, core::Status::NGX_DONE.0 as ngx_int_t);
        }
        core::Status::NGX_DONE
    }
}

/// Body read completion callback
///
/// This callback is invoked by NGINX when the request body has been read.
/// It extracts the body, spawns the async EPP task, and sets up a timer to poll for results.
///
/// # Safety
///
/// This function is called by NGINX with a valid request pointer.
/// It runs in the NGINX worker thread context.
unsafe extern "C" fn body_read_done(r: *mut ngx_http_request_t) {
    ngx_log_debug_raw!(r, "ngx-inference: EPP body_read_done START - r={:p}", r);

    if r.is_null() {
        ngx_log_error_raw!(r, "ngx-inference: EPP body_read_done - request is NULL");
        return;
    }

    // Check connection is valid
    let conn = unsafe { (*r).connection };
    if conn.is_null() {
        ngx_log_error_raw!(r, "ngx-inference: EPP body_read_done - connection is NULL");
        return;
    }

    ngx_log_debug_raw!(r, "ngx-inference: EPP body_read_done - extracting config");

    // Reconstruct context from request configuration (don't use (*r).ctx to avoid free() errors)
    let request: &mut ngx::http::Request = unsafe { ngx::http::Request::from_ngx_http_request(r) };
    let conf = match crate::Module::location_conf(request) {
        Some(c) => c,
        None => {
            ngx_log_error_raw!(r, "ngx-inference: EPP body_read_done: no config found");
            return; // Just return, let NGINX continue
        }
    };

    let upstream_header = if conf.epp_header_name.is_empty() {
        "X-Inference-Upstream".to_string()
    } else {
        conf.epp_header_name.clone()
    };

    let endpoint = match &conf.epp_endpoint {
        Some(e) if !e.is_empty() => e.clone(),
        _ => {
            ngx_log_debug_raw!(
                r,
                "ngx-inference: EPP body_read_done: no endpoint configured"
            );
            return; // Just return, let NGINX continue
        }
    };

    // Collect headers
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in request.headers_in_iterator() {
        if let (Ok(n), Ok(v)) = (name.to_str(), value.to_str()) {
            headers.push((n.to_string(), v.to_string()));
        }
    }

    let epp_ctx = AsyncEppContext {
        endpoint,
        upstream_header,
        timeout_ms: conf.epp_timeout_ms,
        headers,
        use_tls: conf.epp_tls,
        ca_file: conf.epp_ca_file.clone(),
        failure_mode_allow: conf.epp_failure_mode_allow,
        default_upstream: conf.default_upstream.clone(),
    };

    // Extract request body
    let body = match unsafe { extract_request_body(r) } {
        Ok(b) => b,
        Err(e) => {
            ngx_log_error_raw!(r, "ngx-inference: EPP failed to extract body: {}", e);
            unsafe { handle_epp_failure(r, &epp_ctx, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx_int_t) };
            return;
        }
    };

    ngx_log_debug_raw!(
        r,
        "ngx-inference: EPP extracted {} bytes of request body",
        body.len()
    );

    // Create oneshot channel for result
    let (sender, receiver) = oneshot::channel();

    // Spawn async EPP task
    async_processor::spawn_epp_task(epp_ctx.clone(), body, sender);

    ngx_log_debug_raw!(r, "ngx-inference: EPP async task spawned, setting up timer");

    // Create result watcher
    let watcher = Box::new(ResultWatcher::new(receiver, r, epp_ctx.clone()));
    let watcher_ptr = Box::into_raw(watcher);

    // Set up timer to poll for results
    if !unsafe { setup_result_timer(r, watcher_ptr) } {
        ngx_log_error_raw!(r, "ngx-inference: EPP failed to setup result timer");
        unsafe {
            let _ = Box::from_raw(watcher_ptr);
        }
        // Just call failure handler - don't finalize in callback!
        unsafe { handle_epp_failure(r, &epp_ctx, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx_int_t) };
    }
}

/// Extract request body from NGINX request (SAFE HYBRID VERSION)
///
/// This implementation reads from BOTH memory and file buffers using BBR's proven approach.
/// Memory buffers are safe to read in the body_read_done callback context.
///
/// # Safety
///
/// Must be called with valid request pointer in NGINX worker context.
/// Should be called from body_read_done callback when body is freshly read.
unsafe fn extract_request_body(r: *mut ngx_http_request_t) -> Result<Vec<u8>, &'static str> {
    if r.is_null() {
        return Err("null request");
    }

    let req_body = unsafe { (*r).request_body };
    if req_body.is_null() {
        return Ok(Vec::new());
    }

    let body_ref = unsafe { &*req_body };
    let mut bufs = body_ref.bufs;

    if bufs.is_null() {
        return Ok(Vec::new());
    }

    // Get max_body_size from config
    let request: &mut ngx::http::Request = unsafe { ngx::http::Request::from_ngx_http_request(r) };
    let max_body_size = match crate::Module::location_conf(request) {
        Some(conf) => conf.max_body_size,
        None => 10 * 1024 * 1024, // Default 10MB
    };

    let mut body = Vec::new();
    let mut total_read = 0usize;

    // Iterate through buffer chain
    while !bufs.is_null() {
        let chain = unsafe { &*bufs };
        let buf = chain.buf;

        if !buf.is_null() {
            let buf_ref = unsafe { &*buf };

            // Handle memory-backed buffers (copied from BBR's working implementation)
            let pos = buf_ref.pos;
            let last = buf_ref.last;
            if !pos.is_null() && !last.is_null() && last >= pos {
                let len = unsafe { last.offset_from(pos) };

                if len > 0 && len < isize::MAX / 2 {
                    let len_usize = len as usize;
                    let slice = unsafe { std::slice::from_raw_parts(pos as *const u8, len_usize) };
                    body.extend_from_slice(slice);
                    total_read += len_usize;
                }
            }

            // Handle file-backed buffers
            let file = buf_ref.file;
            if !file.is_null() {
                let file_pos = buf_ref.file_pos;
                let file_last = buf_ref.file_last;

                if file_last < file_pos {
                    ngx_log_error_raw!(
                        r,
                        "ngx-inference: EPP file has invalid range: pos={}, last={}",
                        file_pos,
                        file_last
                    );
                    return Err("file has invalid range");
                }

                let file_size = (file_last - file_pos) as usize;

                if file_size > 0 {
                    // Check if adding this file buffer would exceed max_body_size
                    if total_read + file_size > max_body_size {
                        ngx_log_error_raw!(
                            r,
                            "ngx-inference: EPP body size {} exceeds limit {}",
                            total_read + file_size,
                            max_body_size
                        );
                        return Err("body too large");
                    }

                    let fd = unsafe { (*file).fd };
                    if fd != INVALID_FD {
                        // Create buffer for file content
                        let mut file_buffer = vec![0u8; file_size];
                        let mut bytes_read = 0usize;

                        // Read file content in chunks
                        while bytes_read < file_size {
                            let chunk_size =
                                std::cmp::min(FILE_READ_CHUNK_SIZE, file_size - bytes_read);
                            let offset = file_pos.saturating_add(bytes_read as i64);

                            let result = unsafe {
                                libc::pread(
                                    fd,
                                    file_buffer.as_mut_ptr().add(bytes_read) as *mut c_void,
                                    chunk_size,
                                    offset as libc::off_t,
                                )
                            };

                            if result <= 0 {
                                ngx_log_error_raw!(
                                    r,
                                    "ngx-inference: EPP file read error at offset {}, result: {}",
                                    bytes_read,
                                    result
                                );
                                unsafe {
                                    let r_ref = &*r;
                                    if let Some(conn) = r_ref.connection.as_ref() {
                                        ngx::ffi::ngx_log_error_core(
                                            ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                                            conn.log,
                                            0,
                                            #[allow(clippy::manual_c_str_literals)] // FFI code
                                            cstr_ptr(b"ngx-inference: EPP failed to read request body from file\0".as_ptr()),
                                        );
                                    }
                                }
                                return Err("file read error");
                            }
                            bytes_read += result as usize;
                        }

                        if bytes_read > 0 {
                            file_buffer.truncate(bytes_read);
                            body.extend_from_slice(&file_buffer);
                            total_read += bytes_read;
                            ngx_log_info_raw!(
                                r,
                                "ngx-inference: EPP read {} bytes from file, total: {}",
                                bytes_read,
                                total_read
                            );
                        }
                    }
                }
            }
        }

        bufs = chain.next;
    }

    Ok(body)
}

/// Setup timer to poll for EPP results
///
/// # Safety
///
/// Must be called with valid request pointer in NGINX worker context.
unsafe fn setup_result_timer(r: *mut ngx_http_request_t, watcher_ptr: *mut ResultWatcher) -> bool {
    if r.is_null() {
        return false;
    }

    // Get the request's connection
    let conn = unsafe { (*r).connection };
    if conn.is_null() {
        return false;
    }

    // CRITICAL: Allocate timer event from CONNECTION pool
    // Connection pool lives longer than requests and persists until connection closes
    // This is automatically freed when connection is closed
    let conn_pool = unsafe { (*conn).pool };
    let event_ptr = unsafe {
        ngx::ffi::ngx_pcalloc(conn_pool, std::mem::size_of::<ngx_event_t>()) as *mut ngx_event_t
    };

    if event_ptr.is_null() {
        return false;
    }

    // Initialize event
    unsafe {
        (*event_ptr).data = watcher_ptr as *mut _;
        (*event_ptr).handler = Some(check_epp_result);
        (*event_ptr).log = (*conn).log;
    }

    // Add timer
    unsafe {
        ngx_add_timer(event_ptr, TIMER_INTERVAL_MS);
    }

    ngx_log_debug_raw!(
        r,
        "ngx-inference: EPP result timer added at {:p} (conn pool)",
        event_ptr
    );
    true
}

/// Timer callback to check for EPP results
///
/// This is called periodically by NGINX's event loop to check if the async EPP task
/// has completed. It polls the oneshot channel and either reschedules or processes the result.
///
/// # Safety
///
/// This function is called by NGINX with a valid event pointer.
/// It runs in the NGINX worker thread context.
unsafe extern "C" fn check_epp_result(ev: *mut ngx_event_t) {
    if ev.is_null() {
        return;
    }

    let watcher_ptr = unsafe { (*ev).data as *mut ResultWatcher };
    if watcher_ptr.is_null() {
        return;
    }

    // Borrow watcher without taking ownership yet
    let watcher = unsafe { &mut *watcher_ptr };
    let r = watcher.request;

    // Check if request is still valid before proceeding
    if r.is_null() {
        // Request is gone, clean up and return
        unsafe {
            ngx_del_timer(ev);
            let _ = Box::from_raw(watcher_ptr);
            // DON'T free timer event - NGINX manages it
        }
        return;
    }

    // Check if connection is still valid
    let conn = unsafe { (*r).connection };
    if conn.is_null() {
        // Connection is gone, clean up and return
        unsafe {
            ngx_del_timer(ev);
            let _ = Box::from_raw(watcher_ptr);
            // DON'T free timer event - NGINX manages it
        }
        return;
    }

    // Check if request has already been finalized/completed
    // If count is 0, request is being/has been freed
    let count = unsafe { (*r).count() };
    if count == 0 {
        // Request is being freed, clean up timer and return
        unsafe {
            ngx_del_timer(ev);
            let _ = Box::from_raw(watcher_ptr);
            // DON'T free timer event - NGINX manages it
        }
        return;
    }

    // Check for timeout FIRST
    if watcher.is_timed_out() {
        ngx_log_error_raw!(
            r,
            "ngx-inference: EPP timer fired - timeout exceeded ({} ms)",
            watcher.ctx.timeout_ms
        );

        // Delete the timer
        unsafe {
            ngx_del_timer(ev);
        }

        // Clone context before taking ownership
        let ctx = watcher.ctx.clone();

        // Clean up watcher
        let _watcher = unsafe { Box::from_raw(watcher_ptr) };

        // Handle as failure (timeout => 504)
        unsafe { handle_epp_failure(r, &ctx, ngx::ffi::NGX_HTTP_GATEWAY_TIME_OUT as ngx_int_t) };
        return;
    }

    // Try to receive result (non-blocking)
    match watcher.receiver.try_recv() {
        Ok(result) => {
            // Result is ready, process it
            // CRITICAL: Save request pointer BEFORE any cleanup
            let request_ptr = r;

            ngx_log_debug_raw!(
                request_ptr,
                "ngx-inference: EPP timer fired - result received!"
            );

            // Clone context BEFORE taking ownership to avoid lifetime issues
            let ctx = watcher.ctx.clone();

            ngx_log_debug_raw!(request_ptr, "ngx-inference: EPP about to clear event");

            // CRITICAL: Don't call ngx_del_timer - let NGINX clean up the timer
            // Just clear the handler and data so it becomes a no-op if it fires again
            unsafe {
                (*ev).handler = None;
                (*ev).data = std::ptr::null_mut();
            }

            ngx_log_debug_raw!(request_ptr, "ngx-inference: EPP about to drop watcher");

            // Clean up watcher - event is now safe since handler is None
            let _watcher = unsafe { Box::from_raw(watcher_ptr) };

            ngx_log_debug_raw!(
                request_ptr,
                "ngx-inference: EPP watcher dropped, about to process"
            );

            // Process the result with cloned context
            unsafe { process_epp_result(request_ptr, result, &ctx) };

            ngx_log_debug_raw!(
                request_ptr,
                "ngx-inference: EPP process_epp_result returned"
            );
        }
        Err(oneshot::error::TryRecvError::Empty) => {
            // Result not ready yet, reschedule timer
            ngx_log_debug_raw!(
                r,
                "ngx-inference: EPP timer fired - result not ready, rescheduling"
            );
            unsafe {
                ngx_add_timer(ev, TIMER_INTERVAL_MS);
            }
        }
        Err(oneshot::error::TryRecvError::Closed) => {
            // Channel closed without result (task panicked or dropped)
            ngx_log_error_raw!(r, "ngx-inference: EPP timer fired - channel closed");

            // Delete the timer
            unsafe {
                ngx_del_timer(ev);
            }

            // Clean up watcher
            let watcher = unsafe { Box::from_raw(watcher_ptr) };

            // DON'T free the timer event

            unsafe {
                handle_epp_failure(r, &watcher.ctx, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx_int_t)
            };
        }
    }
}

/// Process EPP result and resume request
///
/// # Safety
///
/// Must be called with valid request pointer in NGINX worker context.
unsafe fn process_epp_result(
    r: *mut ngx_http_request_t,
    result: Result<String, String>,
    ctx: &AsyncEppContext,
) {
    ngx_log_debug_raw!(r, "ngx-inference: EPP process_epp_result ENTER");

    match result {
        Ok(upstream) => {
            ngx_log_info_raw!(r, "ngx-inference: EPP selected upstream '{}'", upstream);

            // Set upstream header
            ngx_log_debug_raw!(r, "ngx-inference: EPP about to set header");
            if !unsafe { set_upstream_header(r, &ctx.upstream_header, &upstream) } {
                ngx_log_error_raw!(r, "ngx-inference: EPP failed to set upstream header");
                unsafe { handle_epp_failure(r, ctx, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx_int_t) };
                return;
            }

            ngx_log_debug_raw!(r, "ngx-inference: EPP header set, about to resume phases");
            // Resume request processing
            unsafe {
                ngx_http_core_run_phases(r);
            }
            ngx_log_debug_raw!(r, "ngx-inference: EPP phases resumed");
        }
        Err(e) => {
            ngx_log_error_raw!(r, "ngx-inference: EPP failed: {}", e);
            unsafe { handle_epp_failure(r, ctx, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx_int_t) };
        }
    }
}

/// Handle EPP failure according to failure mode
///
/// # Safety
///
/// Must be called with valid request pointer in NGINX worker context.
unsafe fn handle_epp_failure(
    r: *mut ngx_http_request_t,
    ctx: &AsyncEppContext,
    status_code: ngx_int_t,
) {
    // Clear the post_handler to prevent callback re-execution (like BBR does)
    let req_body = unsafe { (*r).request_body };
    if !req_body.is_null() {
        unsafe { (*req_body).post_handler = None };
    }

    if ctx.failure_mode_allow {
        // Fail-open: set default upstream if available
        ngx_log_debug_raw!(
            r,
            "ngx-inference: EPP fail-open mode, using default upstream"
        );

        if let Some(ref default) = ctx.default_upstream {
            if unsafe { set_upstream_header(r, &ctx.upstream_header, default) } {
                ngx_log_info_raw!(r, "ngx-inference: EPP using default upstream '{}'", default);
            }
        }

        // Resume request processing
        unsafe {
            ngx_http_core_run_phases(r);
        }
    } else {
        // Fail-closed: send error response using special_response_handler (like BBR does)
        ngx_log_error_raw!(
            r,
            "ngx-inference: EPP fail-closed mode, returning error status {}",
            status_code
        );
        unsafe {
            ngx::ffi::ngx_http_special_response_handler(r, status_code);
            ngx::ffi::ngx_http_finalize_request(r, status_code);
        }
    }
}

/// Set upstream header on request
///
/// # Safety
///
/// Must be called with valid request pointer in NGINX worker context.
unsafe fn set_upstream_header(r: *mut ngx_http_request_t, header_name: &str, value: &str) -> bool {
    if r.is_null() {
        return false;
    }

    let _name_cstr = match CString::new(header_name) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let _value_cstr = match CString::new(value) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Allocate name and value from request pool
    let pool = unsafe { (*r).pool };

    let name_len = header_name.len();
    let value_len = value.len();

    let name_ptr = unsafe { ngx::ffi::ngx_pnalloc(pool, name_len) as *mut u8 };
    if name_ptr.is_null() {
        return false;
    }

    let value_ptr = unsafe { ngx::ffi::ngx_pnalloc(pool, value_len) as *mut u8 };
    if value_ptr.is_null() {
        return false;
    }

    // Copy data
    unsafe {
        std::ptr::copy_nonoverlapping(header_name.as_ptr(), name_ptr, name_len);
        std::ptr::copy_nonoverlapping(value.as_ptr(), value_ptr, value_len);
    }

    // Add header to request
    let headers_in = unsafe { &mut (*r).headers_in };
    let header_ptr = unsafe { ngx::ffi::ngx_list_push(&mut headers_in.headers as *mut _) }
        as *mut ngx::ffi::ngx_table_elt_t;

    if header_ptr.is_null() {
        return false;
    }

    unsafe {
        (*header_ptr).hash = 1;
        (*header_ptr).key.len = name_len;
        (*header_ptr).key.data = name_ptr;
        (*header_ptr).value.len = value_len;
        (*header_ptr).value.data = value_ptr;
        (*header_ptr).lowcase_key = std::ptr::null_mut();
    }

    true
}
