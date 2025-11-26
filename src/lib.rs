use std::ffi::{c_char, c_void};

use ngx::core;
use ngx::ffi::{
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_add_variable, ngx_http_handler_pt, ngx_http_module_t,
    ngx_http_phases_NGX_HTTP_ACCESS_PHASE, ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t,
    NGX_CONF_TAKE1, NGX_HTTP_MAIN_CONF, NGX_HTTP_SRV_CONF, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE, NGX_LOG_EMERG,
};
use ngx::http::{self, HttpModule, MergeConfigError};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::{
    http_request_handler, http_variable_get, ngx_conf_log_error, ngx_log_debug_http, ngx_string,
};

/* Internal modules for gRPC ext-proc client and generated protos */
pub mod protos;
pub mod grpc;

/// Helper to get an incoming request header value by name (case-insensitive).
fn get_header_in<'a>(request: &'a http::Request, key: &str) -> Option<&'a str> {
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

// Inference module for Gateway API inference extensions.
// Pipeline (request path):
//   1) Optional BBR (Body-Based Routing): Standard extension that streams request body to
//      remote ext-proc server to detect model name from JSON and set header X-Gateway-Model-Name.
//      Follows the reference BBR implementation in the Gateway API Inference Extension.
//   2) Standard EPP (Endpoint Picker Processor): Follows the reference specification to
//      send request context to remote ext-proc, receive upstream endpoint, and set header
//      X-Inference-Upstream per the Gateway API Inference Extension protocol.
//
// Note: Both EPP and BBR implementations follow the Gateway API Inference Extension
// specification and reference implementations.

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
        let v = ngx_http_add_variable(cf as *mut ngx::ffi::ngx_conf_s, name, 0);
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

#[derive(Debug, Default, Clone)]
struct ModuleConfig {
    // BBR (Body-Based Routing)
    bbr_enable: bool,
    bbr_endpoint: Option<String>, // host:port (plaintext) or scheme://host:port
    bbr_chunk_size: usize,        // 1KB-64KB range (1024-65536 bytes)
    bbr_timeout_ms: u64,
    bbr_failure_mode_allow: bool, // fail-open if ext-proc unavailable
    bbr_header_name: String,      // default "X-Gateway-Model-Name"

    // EPP (Endpoint Picker Processor)
    epp_enable: bool,
    epp_endpoint: Option<String>, // host:port
    epp_timeout_ms: u64,
    epp_failure_mode_allow: bool, // fail-open
    epp_header_name: String,      // default "X-Inference-Upstream"

    // Reserved: limits
    max_body_size_bytes: Option<usize>,
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

impl http::Merge for ModuleConfig {
    fn merge(&mut self, prev: &ModuleConfig) -> Result<(), MergeConfigError> {
        // Inherit enable flags
        if prev.bbr_enable {
            self.bbr_enable = true;
        }
        if prev.epp_enable {
            self.epp_enable = true;
        }

        // Inherit string options if not set
        if self.bbr_endpoint.is_none() {
            self.bbr_endpoint = prev.bbr_endpoint.clone();
        }
        if self.epp_endpoint.is_none() {
            self.epp_endpoint = prev.epp_endpoint.clone();
        }

        // Inherit numeric with defaults
        if self.bbr_chunk_size == 0 {
            self.bbr_chunk_size = if prev.bbr_chunk_size == 0 { 64 * 1024 } else { prev.bbr_chunk_size };
        }
        if self.bbr_timeout_ms == 0 {
            self.bbr_timeout_ms = if prev.bbr_timeout_ms == 0 { 200 } else { prev.bbr_timeout_ms };
        }
        if self.epp_timeout_ms == 0 {
            self.epp_timeout_ms = if prev.epp_timeout_ms == 0 { 200 } else { prev.epp_timeout_ms };
        }
        if self.bbr_header_name.is_empty() {
            self.bbr_header_name = if prev.bbr_header_name.is_empty() {
                "X-Gateway-Model-Name".to_string()
            } else {
                prev.bbr_header_name.clone()
            }
        }
        if self.epp_header_name.is_empty() {
            self.epp_header_name = if prev.epp_header_name.is_empty() {
                "X-Inference-Upstream".to_string()
            } else {
                prev.epp_header_name.clone()
            }
        }

        // Inherit bools
        if prev.bbr_failure_mode_allow {
            self.bbr_failure_mode_allow = true;
        }
        if prev.epp_failure_mode_allow {
            self.epp_failure_mode_allow = true;
        }

        // Inherit limits
        if self.max_body_size_bytes.is_none() {
            self.max_body_size_bytes = prev.max_body_size_bytes;
        }

        Ok(())
    }
}

// -------------------- Directives --------------------

fn set_on_off(val: &str) -> Option<bool> {
    if val.eq_ignore_ascii_case("on") {
        Some(true)
    } else if val.eq_ignore_ascii_case("off") {
        Some(false)
    } else {
        None
    }
}

fn set_string_opt(target: &mut Option<String>, val: &str) {
    if !val.is_empty() {
        *target = Some(val.to_string());
    }
}

fn set_usize(target: &mut usize, val: &str) -> Result<(), ()> {
    match val.parse::<usize>() {
        Ok(n) => {
            *target = n;
            Ok(())
        }
        Err(_) => Err(()),
    }
}

fn set_u64(target: &mut u64, val: &str) -> Result<(), ()> {
    match val.parse::<u64>() {
        Ok(n) => {
            *target = n;
            Ok(())
        }
        Err(_) => Err(()),
    }
}

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

// inference_bbr_endpoint host:port
extern "C" fn ngx_http_inference_set_bbr_endpoint(
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_endpoint` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        set_string_opt(&mut conf.bbr_endpoint, val);
    }
    core::NGX_CONF_OK
}

// inference_bbr_chunk_size N (1024-65536 bytes, for streaming body chunks to BBR server)
extern "C" fn ngx_http_inference_set_bbr_chunk_size(
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_chunk_size` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        match set_usize(&mut conf.bbr_chunk_size, val) {
            Ok(()) => {
                // Validate chunk size is within reasonable bounds (1KB to 64KB)
                if conf.bbr_chunk_size < 1024 || conf.bbr_chunk_size > 65536 {
                    ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_chunk_size` must be between 1024 and 65536 bytes");
                    return core::NGX_CONF_ERROR;
                }
            }
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_chunk_size` must be usize");
                return core::NGX_CONF_ERROR;
            }
        }
    }
    core::NGX_CONF_OK
}

// inference_bbr_timeout_ms N (gRPC timeout for BBR server communication, default 200ms)
extern "C" fn ngx_http_inference_set_bbr_timeout_ms(
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_timeout_ms` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        if set_u64(&mut conf.bbr_timeout_ms, val).is_err() {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_timeout_ms` must be u64");
            return core::NGX_CONF_ERROR;
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_failure_mode_allow` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.bbr_failure_mode_allow = b,
            None => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_bbr_failure_mode_allow` expects on|off");
                return core::NGX_CONF_ERROR;
            }
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_failure_mode_allow` not utf-8");
                return core::NGX_CONF_ERROR;
            }
        };

        match set_on_off(val) {
            Some(b) => conf.epp_failure_mode_allow = b,
            None => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`inference_epp_failure_mode_allow` expects on|off");
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
static mut NGX_HTTP_INFERENCE_COMMANDS: [ngx_command_t; 12] = [
    ngx_command_t {
        name: ngx_string!("inference_bbr"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_endpoint"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_endpoint),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_chunk_size"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_chunk_size),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_timeout_ms"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_timeout_ms),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_failure_mode_allow"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_failure_mode_allow),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_bbr_header_name"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_bbr_header_name),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_endpoint"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_endpoint),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_timeout_ms"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_timeout_ms),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_failure_mode_allow"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(ngx_http_inference_set_epp_failure_mode_allow),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("inference_epp_header_name"),
        type_: ((NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF) | NGX_CONF_TAKE1) as ngx_uint_t,
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

http_variable_get!(inference_upstream_var_get, |request: &mut http::Request, v: *mut ngx::ffi::ngx_variable_value_t, _data: usize| {
    // Evaluate $inference_upstream from "X-Inference-Upstream" header
    unsafe {
        let conf = Module::location_conf(request).expect("module config missing");
        let upstream_header = if conf.epp_header_name.is_empty() { "X-Inference-Upstream".to_string() } else { conf.epp_header_name.clone() };
        if let Some(val) = get_header_in(request, &upstream_header) {
            let bytes = val.as_bytes();
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
});

 // -------------------- Access Phase Handler --------------------

http_request_handler!(inference_access_handler, |request: &mut http::Request| {
    let conf = Module::location_conf(request).expect("module config missing");

    ngx_log_debug_http!(
        request,
        "ngx-inference: bbr_enable={} epp_enable={}",
        conf.bbr_enable,
        conf.epp_enable
    );

    // Stage 1: BBR (Body-Based Routing) - stream request body to ext-proc for JSON model detection
    if conf.bbr_enable {
        // Initiate asynchronous reading of the client request body.
        unsafe {
            // If header already present, skip BBR.
            let header_name = if conf.bbr_header_name.is_empty() { "X-Gateway-Model-Name".to_string() } else { conf.bbr_header_name.clone() };
            if let Some(_existing) = get_header_in(request, &header_name) {
                // Already set, skip BBR stage.
            } else {
                let r_ptr: *mut ngx::ffi::ngx_http_request_t = request.as_mut();
                let rc = ngx::ffi::ngx_http_read_client_request_body(r_ptr, Some(bbr_body_read_handler));
                if rc == core::Status::NGX_AGAIN.into() {
                    // Body will be read asynchronously; resume processing in the handler.
                    return core::Status::NGX_DONE;
                } else if rc == core::Status::NGX_OK.into() {
                    // Body has been read synchronously; run handler immediately.
                    bbr_body_read_handler(r_ptr);
                } else {
                    ngx_log_debug_http!(request, "ngx-inference: BBR read_body rc={}", rc);
                    if !conf.bbr_failure_mode_allow {
                        // Fail closed: reject request
                        return http::HTTPStatus::BAD_GATEWAY.into();
                    }
                }
            }
        }
    }

    // Stage 2: EPP (Endpoint Picker Processor) - headers-only exchange for upstream selection
    if conf.epp_enable {
        match epp_pick_upstream(request, conf) {
            Ok(()) => {
                // upstream header set
            }
            Err(err) => {
                ngx_log_debug_http!(request, "ngx-inference: EPP error: {}", err);
                if !conf.epp_failure_mode_allow {
                    // Fail closed
                    return http::HTTPStatus::BAD_GATEWAY.into();
                }
            }
        }
    }

    // Continue normal processing
    core::Status::NGX_DECLINED
});

 // -------------------- Core Pipeline Implementation --------------------

// Body read handler: called after ngx_http_read_client_request_body finishes reading.
extern "C" fn bbr_body_read_handler(r: *mut ngx::ffi::ngx_http_request_t) {
    unsafe {
        // Reconstruct Rust wrapper and config
        let request: &mut http::Request = ngx::http::Request::from_ngx_http_request(r);
        let conf = Module::location_conf(request).expect("module config missing");

        // Validate endpoint
        let endpoint = match &conf.bbr_endpoint {
            Some(e) if !e.is_empty() => e.clone(),
            _ => {
                // No endpoint configured; resume normal processing
                ngx::ffi::ngx_http_core_run_phases(r);
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
        if let Some(_existing) = get_header_in(request, &header_name) {
            ngx::ffi::ngx_http_core_run_phases(r);
            return;
        }

        // Collect in-memory request body buffers into a contiguous Vec<u8>.
        // Pre-allocate based on Content-Length if available for better performance.
        // Note: For large bodies NGINX may spill to temp files; this implementation
        // handles in-memory buffers and can be extended to read file-backed buffers.
        let content_length = get_header_in(request, "Content-Length")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body: Vec<u8> = Vec::with_capacity(content_length);

        let rb = (*r).request_body;
        if !rb.is_null() {
            let mut cl = (*rb).bufs;
            while !cl.is_null() {
                let buf = (*cl).buf;
                if buf.is_null() {
                    break;
                }
                let pos = (*buf).pos;
                let last = (*buf).last;
                if !pos.is_null() && !last.is_null() {
                    let len = last.offset_from(pos) as usize;
                    if len > 0 {
                        let slice = std::slice::from_raw_parts(pos as *const u8, len);
                        body.extend_from_slice(slice);
                    }
                }
                // TODO: handle file-backed buffers (buf->file != NULL) by reading ranges [file_pos, file_last].
                cl = (*cl).next;
            }
        }

        // Stream body to ext-proc and read returned header.
        match crate::grpc::bbr_stream_blocking_with_body(
            endpoint.as_str(),
            &body,
            conf.bbr_chunk_size,
            &header_name,
            conf.bbr_timeout_ms,
        ) {
            Ok(Some(val)) => {
                let _ = request.add_header_in(&header_name, &val);
            }
            Ok(None) => {
                // no header returned; proceed
            }
            Err(err) => {
                // Log error; if fail-closed desired, we would have to finalize with BAD_GATEWAY here.
                ngx_log_debug_http!(request, "ngx-inference: BBR gRPC error in handler: {}", err);
                if !conf.bbr_failure_mode_allow {
                    // Fail closed: set 502 and finalize
                    request.set_status(http::HTTPStatus::BAD_GATEWAY);
                    ngx::ffi::ngx_http_finalize_request(r, ngx::ffi::NGX_HTTP_BAD_GATEWAY as ngx::ffi::ngx_int_t);
                    return;
                }
            }
        }

        // Resume normal HTTP processing phases after asynchronous operation.
        ngx::ffi::ngx_http_core_run_phases(r);
    }
}

fn epp_pick_upstream(request: &mut http::Request, conf: &ModuleConfig) -> Result<(), &'static str> {
    // If EPP endpoint is not configured, skip.
    let endpoint = match &conf.epp_endpoint {
        Some(e) if !e.is_empty() => e.as_str(),
        _ => return Ok(()),
    };

    let upstream_header = if conf.epp_header_name.is_empty() { "X-Inference-Upstream".to_string() } else { conf.epp_header_name.clone() };
    let upstream_header_str = upstream_header.as_str();

    // If upstream already set (e.g., previous stage), skip.
    if get_header_in(request, upstream_header_str).is_some() {
        return Ok(());
    }

    // Call gRPC client: forward incoming headers to EPP for richer context; headers-only request to pick upstream.
    let mut hdrs: Vec<(String, String)> = Vec::new();
    for (name, value) in request.headers_in_iterator() {
        if let (Ok(n), Ok(v)) = (name.to_str(), value.to_str()) {
            hdrs.push((n.to_string(), v.to_string()));
        }
    }
    match crate::grpc::epp_headers_blocking(endpoint, conf.epp_timeout_ms, upstream_header_str, hdrs) {
        Ok(Some(val)) => {
            // Write upstream selection header for variable consumption.
            let _ = request.add_header_in(upstream_header_str, &val);
        }
        Ok(None) => {
            // No upstream provided
        }
        Err(_err) => {
            return Err("epp grpc error");
        }
    }

    Ok(())
}
