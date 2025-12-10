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
use ngx::{http_request_handler, http_variable_get, ngx_conf_log_error, ngx_string};

/* Internal modules for gRPC ext-proc client and generated protos */
pub mod epp;
pub mod grpc;
pub mod model_extractor;
pub mod modules;
pub mod protos;

use modules::bbr::get_header_in;
use modules::config::{set_on_off, set_string_opt, set_u64, set_usize};
use modules::{BbrProcessor, EppProcessor, ModuleConfig};

// Platform-agnostic string pointer casting for nginx FFI
// c_char can be either i8 or u8 depending on platform
#[inline]
fn cstr_ptr(s: *const u8) -> *const c_char {
    s.cast::<c_char>()
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
        let cf_ref = unsafe { &mut *cf };
        // Allocate variable name from configuration pool
        let name = unsafe { &mut ngx_str_t::from_str(cf_ref.pool, "inference_upstream") as *mut _ };
        // Add variable with no special flags
        let v = unsafe { ngx_http_add_variable(cf, name, 0) };
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
        let cf = unsafe { &mut *cf };
        let cmcf = NgxHttpCoreModule::main_conf_mut(cf).expect("http core main conf");

        // Register an Access phase handler to run before upstream selection.
        let h = unsafe {
            ngx_array_push(
                &mut cmcf.phases[ngx_http_phases_NGX_HTTP_ACCESS_PHASE as usize].handlers,
            ) as *mut ngx_http_handler_pt
        };
        if h.is_null() {
            return core::Status::NGX_ERROR.into();
        }
        unsafe { *h = Some(inference_access_handler) };
        core::Status::NGX_OK.into()
    }
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

// -------------------- Directives --------------------

// Macro to generate configuration directive handlers with reduced boilerplate
macro_rules! ngx_conf_handler {
    // Handler for on/off values
    (on_off, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let val = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` argument is not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    match set_on_off(val) {
                        Some(b) => conf.$field = b,
                        None => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` expects on|off"));
                            return core::NGX_CONF_ERROR;
                        }
                    }
                }
                core::NGX_CONF_OK
            }
        }
    };

    // Handler for optional string values
    (string_opt, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let val = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    set_string_opt(&mut conf.$field, val);
                }
                core::NGX_CONF_OK
            }
        }
    };

    // Handler for required string values
    (string, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let val = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    conf.$field = val.to_string();
                }
                core::NGX_CONF_OK
            }
        }
    };

    // Handler for usize values
    (usize, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let val = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    if set_usize(&mut conf.$field, val).is_err() {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` must be usize"));
                        return core::NGX_CONF_ERROR;
                    }
                }
                core::NGX_CONF_OK
            }
        }
    };

    // Handler for u64 values
    (u64, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let val = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    if set_u64(&mut conf.$field, val).is_err() {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` must be u64"));
                        return core::NGX_CONF_ERROR;
                    }
                }
                core::NGX_CONF_OK
            }
        }
    };

    // Handler for Option<String> path values
    (path, $name:literal, $field:ident) => {
        paste::paste! {
            extern "C" fn [<ngx_http_inference_set_ $field>](
                cf: *mut ngx_conf_t,
                _cmd: *mut ngx_command_t,
                conf: *mut c_void,
            ) -> *mut c_char {
                unsafe {
                    if cf.is_null() || conf.is_null() {
                        return core::NGX_CONF_ERROR;
                    }
                    let cf_ref = &mut *cf;
                    if cf_ref.args.is_null() {
                        return core::NGX_CONF_ERROR;
                    }

                    let conf = &mut *(conf as *mut ModuleConfig);
                    let args: &[ngx_str_t] = (*cf_ref.args).as_slice();

                    // Defensive check: ensure we have at least 2 args (directive name + value)
                    if args.len() < 2 {
                        ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` missing argument"));
                        return core::NGX_CONF_ERROR;
                    }

                    let path = match args[1].to_str() {
                        Ok(s) => s,
                        Err(_) => {
                            ngx_conf_log_error!(NGX_LOG_EMERG, cf, concat!("`", $name, "` argument is not utf-8"));
                            return core::NGX_CONF_ERROR;
                        }
                    };

                    conf.$field = Some(path.to_string());
                }
                core::NGX_CONF_OK
            }
        }
    };
}

// Generate all configuration handlers using the macro
ngx_conf_handler!(on_off, "inference_bbr", bbr_enable);
ngx_conf_handler!(usize, "inference_max_body_size", max_body_size);
ngx_conf_handler!(string, "inference_bbr_header_name", bbr_header_name);
ngx_conf_handler!(string, "inference_bbr_default_model", bbr_default_model);
ngx_conf_handler!(string_opt, "inference_default_upstream", default_upstream);
ngx_conf_handler!(on_off, "inference_epp", epp_enable);
ngx_conf_handler!(string_opt, "inference_epp_endpoint", epp_endpoint);
ngx_conf_handler!(u64, "inference_epp_timeout_ms", epp_timeout_ms);
ngx_conf_handler!(
    on_off,
    "inference_epp_failure_mode_allow",
    epp_failure_mode_allow
);
ngx_conf_handler!(string, "inference_epp_header_name", epp_header_name);
ngx_conf_handler!(on_off, "inference_epp_tls", epp_tls);
ngx_conf_handler!(path, "inference_epp_ca_file", epp_ca_file);

// NGINX directives table
// SAFETY: Must be `static mut` because ngx_command_t contains raw pointers (*mut c_void, *mut u8)
// which don't implement Sync, preventing use of immutable `static`. However, this is only written
// during module initialization (single-threaded) and only read afterwards. nginx expects a mutable
// pointer but never mutates it after initialization.
static mut NGX_HTTP_INFERENCE_COMMANDS: [ngx_command_t; 13] = [
    ngx_command_t {
        name: ngx_string!("inference_default_upstream"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_default_upstream),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
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
        name: ngx_string!("inference_max_body_size"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_max_body_size),
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
    ngx_command_t {
        name: ngx_string!("inference_epp_tls"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_tls),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_ca_file"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1)
            as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_ca_file),
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

/// Helper function to allocate and set variable value from bytes
///
/// # Safety
///
/// This function must be called with the following guarantees:
/// - `v` must be a valid, non-null pointer to an initialized `ngx_variable_value_t`
/// - `v` must be properly aligned and point to valid memory
/// - `pool` must be a valid nginx pool that will outlive the variable's lifetime
/// - `bytes` must contain valid UTF-8 data that doesn't exceed u32::MAX in length
/// - Caller must ensure no concurrent access to `*v` while this function executes
#[inline]
unsafe fn set_variable_from_bytes(
    v: *mut ngx::ffi::ngx_variable_value_t,
    pool: &ngx::core::Pool,
    bytes: &[u8],
) -> core::Status {
    unsafe {
        if bytes.is_empty() {
            (*v).set_not_found(1);
            (*v).set_len(0);
            (*v).data = ::core::ptr::null_mut();
            return core::Status::NGX_OK;
        }

        // Check for length overflow before casting to u32
        if bytes.len() > u32::MAX as usize {
            (*v).set_not_found(1);
            (*v).set_len(0);
            (*v).data = ::core::ptr::null_mut();
            return core::Status::NGX_ERROR;
        }

        let data_ptr = pool.alloc(bytes.len());
        if data_ptr.is_null() {
            // mark not found on allocation error
            (*v).set_not_found(1);
            (*v).set_len(0);
            (*v).data = ::core::ptr::null_mut();
            return core::Status::NGX_ERROR;
        }

        ::core::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr as *mut u8, bytes.len());

        // set ngx_variable_value_t fields
        // SAFETY: Length checked above to fit in u32
        (*v).set_len(bytes.len() as u32);
        (*v).set_valid(1);
        (*v).set_no_cacheable(0);
        (*v).set_escape(0);
        (*v).set_not_found(0);
        (*v).data = data_ptr as *mut u8;

        core::Status::NGX_OK
    }
}

http_variable_get!(
    inference_upstream_var_get,
    |request: &mut http::Request, v: *mut ngx::ffi::ngx_variable_value_t, _data: usize| {
        // Evaluate $inference_upstream from "X-Inference-Upstream" header
        // SAFETY: nginx guarantees request is non-null when calling variable handlers.
        // The http_variable_get! macro converts the raw pointer to a reference.
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
            let pool = request.pool();

            if let Some(val) = get_header_in(request, &upstream_header) {
                return set_variable_from_bytes(v, &pool, val.as_bytes());
            } else if let Some(ref default_upstream) = conf.default_upstream {
                return set_variable_from_bytes(v, &pool, default_upstream.as_bytes());
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
//
// Module Processing Pipeline:
// ===========================
// This handler runs in the ACCESS phase before upstream selection. It executes two
// optional stages in sequence:
//
// 1. BBR (Body-Based Routing) - Extracts model name from request body
//    - Reads request body (may be async)
//    - Parses JSON to find "model" field
//    - Sets X-Gateway-Model-Name header (or configured header name)
//    - Can fail with 413 if body exceeds max_body_size
//
// 2. EPP (Endpoint Picker Processor) - Selects upstream endpoint
//    - Sends request metadata to external gRPC service
//    - Receives upstream endpoint selection
//    - Sets X-Inference-Upstream header (or configured header name)
//
// Error Handling Strategy:
// ========================
// - BBR errors (except 413): Return HTTP 500, request terminates
// - BBR 413 error: Return NGX_OK (request already finalized), proceeds to log phase
// - EPP errors with fail-closed mode: Return HTTP 502 (Bad Gateway), or 504 on timeout; request terminates
// - EPP errors with fail-open mode: Log error, continue processing (uses default_upstream if set)
// - If BBR fails fatally, EPP never runs
// - If BBR succeeds and EPP fails (fail-open), request continues to upstream with BBR headers
//
// Return Codes:
// =============
// - NGX_DONE: BBR started async body read, callback will resume processing
// - NGX_OK: Request already finalized (e.g., 413 sent), proceed to log phase
// - NGX_DECLINED: Processing complete, continue to next nginx phase (content/proxy)
// - HTTP_500: Fatal error, nginx will send error response

http_request_handler!(inference_access_handler, |request: &mut http::Request| {
    let conf = match Module::location_conf(request) {
        Some(c) => c,
        None => {
            // Missing config is a fatal setup issue - fail the request
            unsafe {
                let r = request.as_mut();
                if let Some(conn) = r.connection.as_ref() {
                    let msg = b"ngx-inference: module config missing, cannot process request\0";
                    ngx::ffi::ngx_log_error_core(
                        ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                        conn.log,
                        0,
                        cstr_ptr(msg.as_ptr()),
                    );
                }
            }
            return http::HTTPStatus::INTERNAL_SERVER_ERROR.into();
        }
    };

    // No routine logging - only log errors and warnings

    // Stage 1: BBR (Body-Based Routing)
    // If this fails, EPP will NOT run (request terminates or is already finalized)
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
                    // IMPORTANT: Return NGX_OK here, not an error status.
                    // When BBR processing detects oversized body and sets 413:
                    // - The request has already been finalized via ngx_http_finalize_request()
                    // - Response headers and special response handler were already triggered
                    // - Returning NGX_OK tells nginx: "access phase complete, proceed to next phase"
                    // - The next phase will see the finalized status and skip to log phase
                    // - Returning an error here would cause nginx to send *another* error response
                    return core::Status::NGX_OK;
                }
                // Otherwise continue processing
            }
            core::Status::NGX_ERROR => {
                // Other BBR error - return 500 Internal Server Error
                return http::HTTPStatus::INTERNAL_SERVER_ERROR.into();
            }
            _ => {
                // Continue processing
            }
        }
    }

    // Stage 2: EPP (Endpoint Picker Processor) - headers-only exchange for upstream selection
    if conf.epp_enable {
        match EppProcessor::process_request(request, conf) {
            core::Status::NGX_DECLINED => {
                // EPP processed successfully or was skipped, continue
            }
            core::Status::NGX_DONE => {
                // EPP started async processing, suspend request
                return core::Status::NGX_DONE;
            }
            core::Status::NGX_ERROR => {
                unsafe {
                    let r = request.as_mut();
                    if let Some(conn) = r.connection.as_ref() {
                        let msg = b"ngx-inference: EPP module processing failed internally\0";
                        ngx::ffi::ngx_log_error_core(
                            ngx::ffi::NGX_LOG_ERR as ngx::ffi::ngx_uint_t,
                            conn.log,
                            0,
                            cstr_ptr(msg.as_ptr()),
                        );
                    }
                }
                if !conf.epp_failure_mode_allow {
                    // Fail closed
                    unsafe {
                        let r = request.as_mut();
                        if let Some(conn) = r.connection.as_ref() {
                            ngx::ffi::ngx_log_error_core(
                                ngx::ffi::NGX_LOG_WARN as ngx::ffi::ngx_uint_t,
                                conn.log,
                                0,
                                #[allow(clippy::manual_c_str_literals)] // FFI code
                                cstr_ptr(b"ngx-inference: Module returning HTTP 502 (Bad Gateway) due to EPP processing failure (fail-closed mode)\0".as_ptr()),
                            );
                        }
                    }
                    return http::HTTPStatus::BAD_GATEWAY.into();
                }
            }
            _ => {
                // Other status, continue processing
            }
        }
    }

    // Continue normal processing
    core::Status::NGX_DECLINED
});

// Module configuration and command definitions...
