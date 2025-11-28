use std::ffi::{c_char, c_void};

use ngx::core;
use ngx::ffi::{
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_add_variable, ngx_http_handler_pt,
    ngx_http_module_t, ngx_http_phases_NGX_HTTP_ACCESS_PHASE, ngx_int_t, ngx_module_t, ngx_str_t,
    ngx_uint_t, NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MAIN_CONF,
    NGX_HTTP_MODULE, NGX_HTTP_SRV_CONF, NGX_LOG_EMERG,
};
use ngx::http::{self, HttpModule};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::{
    http_request_handler, http_variable_get, ngx_conf_log_error, ngx_log_debug_http, ngx_string,
};

/* Internal modules for gRPC ext-proc client and generated protos */
pub mod grpc;
pub mod model_extractor;
pub mod modules;
pub mod protos;

use modules::bbr::get_header_in;
use modules::config::{set_on_off, set_string_opt, set_u64, set_usize};
use modules::{BbrProcessor, EppProcessor, ModuleConfig};

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
    s as *const u8 as *const c_char
}

// NGINX module for Gateway API inference extensions.
// Pipeline (request path):
//   1) Optional BBR (Body-Based Routing): Parses JSON request bodies to detect model names
//      and sets X-Gateway-Model-Name header following Gateway API Inference Extension spec.
//   2) Optional EPP (Endpoint Picker Processor): Sends request context to remote ext-proc,
//      receives upstream endpoint, and sets X-Inference-Upstream header per Gateway API spec.
//
// Both BBR and EPP follow the Gateway API Inference Extension specification.

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { &*::core::ptr::addr_of!(ngx_http_inference_module) }
    }

    unsafe extern "C" fn preconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        // Register $inference_upstream variable so it can be used in NGINX config (e.g. proxy_pass http://$inference_upstream;)
        let cf_ref = &mut *cf;
        // Allocate variable name from configuration pool
        let name = &mut ngx_str_t::from_str(cf_ref.pool, "inference_upstream") as *mut _;
        // Add variable with no special flags
        let v = ngx_http_add_variable(cf, name, 0);
        if v.is_null() {
            return core::Status::NGX_ERROR.into();
        }
        // Attach evaluator handler
        unsafe {
            (*v).get_handler = Some(inference_upstream_var_get);
            (*v).data = 0;
        }
        core::Status::NGX_OK.into()
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        // SAFETY: called by NGINX with non-null cf
        let cf = &mut *cf;
        let cmcf = NgxHttpCoreModule::main_conf_mut(cf).expect("http core main conf");

        // Register an Access phase handler to run before upstream selection.
        let h = ngx_array_push(
            &mut cmcf.phases[ngx_http_phases_NGX_HTTP_ACCESS_PHASE as usize].handlers,
        ) as *mut ngx_http_handler_pt;
        if h.is_null() {
            return core::Status::NGX_ERROR.into();
        }
        *h = Some(inference_access_handler);
        core::Status::NGX_OK.into()
    }
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

// -------------------- Directives --------------------

// inference_bbr on|off
extern "C" fn ngx_http_inference_set_bbr_enable(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr` argument is not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.bbr_enable = b,
            None => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr` expects on|off");
                return core::NGX_CONF_ERROR;
            }
        }
    }
    core::NGX_CONF_OK
}

// inference_bbr_failure_mode_allow on|off
extern "C" fn ngx_http_inference_set_bbr_failure_mode_allow(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`inference_bbr_failure_mode_allow` not utf-8"
                );
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.bbr_failure_mode_allow = b,
            None => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`inference_bbr_failure_mode_allow` expects on|off"
                );
                return core::NGX_CONF_ERROR;
            }
        }
    }
    core::NGX_CONF_OK
}

// inference_bbr_max_body_size N (maximum body size in bytes for BBR processing, default 10MB)
extern "C" fn ngx_http_inference_set_bbr_max_body_size(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_max_body_size` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        if set_usize(&mut conf.bbr_max_body_size, val).is_err() {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`inference_bbr_max_body_size` must be usize"
            );
            return core::NGX_CONF_ERROR;
        }
    }
    core::NGX_CONF_OK
}

// inference_bbr_header_name NAME
extern "C" fn ngx_http_inference_set_bbr_header_name(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_header_name` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        conf.bbr_header_name = val.to_string();
    }
    core::NGX_CONF_OK
}

// inference_bbr_default_model MODEL_NAME
extern "C" fn ngx_http_inference_set_bbr_default_model(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_default_model` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };
        conf.bbr_default_model = val.to_string();
    }
    core::NGX_CONF_OK
}

// inference_epp on|off
extern "C" fn ngx_http_inference_set_epp_enable(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp` argument is not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.epp_enable = b,
            None => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp` expects on|off");
                return core::NGX_CONF_ERROR;
            }
        }
    }
    core::NGX_CONF_OK
}

// inference_epp_endpoint host:port
extern "C" fn ngx_http_inference_set_epp_endpoint(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_endpoint` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        set_string_opt(&mut conf.epp_endpoint, val);
    }
    core::NGX_CONF_OK
}

// inference_epp_timeout_ms N (gRPC timeout for EPP server communication, default 200ms)
extern "C" fn ngx_http_inference_set_epp_timeout_ms(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_timeout_ms` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        if set_u64(&mut conf.epp_timeout_ms, val).is_err() {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_timeout_ms` must be u64");
            return core::NGX_CONF_ERROR;
        }
    }
    core::NGX_CONF_OK
}

// inference_epp_failure_mode_allow on|off
extern "C" fn ngx_http_inference_set_epp_failure_mode_allow(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`inference_epp_failure_mode_allow` not utf-8"
                );
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.epp_failure_mode_allow = b,
            None => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`inference_epp_failure_mode_allow` expects on|off"
                );
                return core::NGX_CONF_ERROR;
            }
        }
    }
    core::NGX_CONF_OK
}

/* Set the EPP upstream header name (default "X-Inference-Upstream") */
extern "C" fn ngx_http_inference_set_epp_header_name(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_header_name` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        conf.epp_header_name = val.to_string();
    }
    core::NGX_CONF_OK
}

// NGINX directives table
static mut NGX_HTTP_INFERENCE_COMMANDS: [ngx_command_t; 11] = [
    ngx_command_t {
        name: ngx_string!("inference_bbr"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_max_body_size"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_max_body_size),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_failure_mode_allow"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_failure_mode_allow),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_header_name"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_header_name),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_default_model"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_default_model),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_endpoint"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_endpoint),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_timeout_ms"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_timeout_ms),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_failure_mode_allow"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_failure_mode_allow),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_header_name"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_header_name),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

static NGX_HTTP_INFERENCE_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

// Generate the `ngx_modules` table with exported modules when building as cdylib.
#[cfg(feature = "export-modules")]
ngx::ngx_modules!(ngx_http_inference_module);

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), no_mangle)]
pub static mut ngx_http_inference_module: ngx_module_t = ngx_module_t {
    ctx: std::ptr::addr_of!(NGX_HTTP_INFERENCE_MODULE_CTX) as _,
    commands: unsafe { &NGX_HTTP_INFERENCE_COMMANDS[0] as *const _ as *mut _ },
    type_: NGX_HTTP_MODULE as _,
    ..ngx_module_t::default()
};

// -------------------- Variable: $inference_upstream --------------------
// Exposes the value of the "X-Inference-Upstream" header set by EPP for upstream selection.
// Usage: proxy_pass http://$inference_upstream; (configured endpoint from EPP response)

http_variable_get!(
    inference_upstream_var_get,
    |request: &mut http::Request, v: *mut ngx::ffi::ngx_variable_value_t, _data: usize| {
        // Evaluate $inference_upstream from "X-Inference-Upstream" header
        unsafe {
            if v.is_null() {
                return core::Status::NGX_ERROR;
            }
            let conf = match Module::location_conf(request) {
                Some(c) => c,
                None => {
                    // mark not found on missing config
                    (*v).set_not_found(1);
                    (*v).set_len(0);
                    (*v).data = ::core::ptr::null_mut();
                    return core::Status::NGX_OK;
                }
            };
            let upstream_header = if conf.epp_header_name.is_empty() {
                "X-Inference-Upstream".to_string()
            } else {
                conf.epp_header_name.clone()
            };
            if let Some(val) = get_header_in(request, &upstream_header) {
                let bytes = val.as_bytes();
                if bytes.is_empty() {
                    (*v).set_not_found(1);
                    (*v).set_len(0);
                    (*v).data = ::core::ptr::null_mut();
                    return core::Status::NGX_OK;
                }
                // allocate buffer from request pool
                let pool = request.pool();
                let data_ptr = pool.alloc(bytes.len());
                if data_ptr.is_null() {
                    // mark not found on error
                    (*v).set_not_found(1);
                    (*v).set_len(0);
                    (*v).data = ::core::ptr::null_mut();
                    return core::Status::NGX_ERROR;
                }
                ::core::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr as *mut u8, bytes.len());
                // set ngx_variable_value_t fields
                (*v).set_len(bytes.len() as u32);
                (*v).set_valid(1);
                (*v).set_no_cacheable(0);
                (*v).set_escape(0);
                (*v).set_not_found(0);
                (*v).data = data_ptr as *mut u8;
            } else {
                // mark variable as not found
                (*v).set_not_found(1);
                (*v).set_len(0);
                (*v).data = ::core::ptr::null_mut();
            }
        }
        core::Status::NGX_OK
    }
);

// -------------------- Access Phase Handler --------------------

http_request_handler!(inference_access_handler, |request: &mut http::Request| {
    let conf = match Module::location_conf(request) {
        Some(c) => c,
        None => {
            // Use error level for missing config since it's a setup issue
            unsafe {
                let msg = b"ngx-inference: module config missing\0";
                ngx::ffi::ngx_log_error_core(
                    ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                    request.as_mut().connection.as_ref().unwrap().log,
                    0,
                    cstr_ptr(msg.as_ptr()),
                );
            }
            return core::Status::NGX_DECLINED;
        }
    };

    // No routine logging - only log errors and warnings

    // Stage 1: BBR (Body-Based Routing)
    if conf.bbr_enable {
        let bbr_status = BbrProcessor::process_request(request, conf);
        match bbr_status {
            core::Status::NGX_DONE => {
                // Body reading started, handler will be called later
                return core::Status::NGX_DONE;
            }
            core::Status::NGX_OK => {
                // Check if request was finalized (e.g., 413 error)
                let response_status = request.as_mut().headers_out.status;
                if response_status
                    == ngx::ffi::NGX_HTTP_REQUEST_ENTITY_TOO_LARGE as ngx::ffi::ngx_uint_t
                {
                    // Request was finalized with 413, don't continue processing
                    return core::Status::NGX_OK;
                }
                // Otherwise continue processing
            }
            core::Status::NGX_ERROR => {
                // Other error - check failure mode
                if !conf.bbr_failure_mode_allow {
                    return http::HTTPStatus::BAD_GATEWAY.into();
                }
            }
            _ => {
                // Continue processing
            }
        }
    }

    // Stage 2: EPP (Endpoint Picker Processor) - headers-only exchange for upstream selection
    if conf.epp_enable {
        match EppProcessor::process_request(request, conf) {
            Ok(()) => {
                // upstream header set
            }
            Err(err) => {
                ngx_log_debug_http!(request, "ngx-inference: EPP error: {}", err);
                if !conf.epp_failure_mode_allow {
                    // Fail closed
                    unsafe {
                        let r_ptr: *mut ngx::ffi::ngx_http_request_t = request.as_mut();
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                            (*(*r_ptr).connection).log,
                            0,
                            #[allow(clippy::manual_c_str_literals)] // FFI code
                            cstr_ptr(b"ngx-inference: EPP rejected request with HTTP 502 - external processor error\0".as_ptr()),
                        );
                    }
                    return http::HTTPStatus::BAD_GATEWAY.into();
                }
            }
        }
    }

    // Continue normal processing
    core::Status::NGX_DECLINED
});

// Module configuration and command definitions...
