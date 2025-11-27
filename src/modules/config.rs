use ngx::http::MergeConfigError;

/// Configuration structure for the ngx-inference module
#[derive(Clone)]
pub struct ModuleConfig {
    // BBR (Body-Based Routing) - implemented directly in module
    pub bbr_enable: bool,
    pub bbr_failure_mode_allow: bool, // fail-open if JSON parsing fails
    pub bbr_header_name: String,      // default "X-Gateway-Model-Name"
    pub bbr_max_body_size: usize,     // default 10MB limit for BBR processing
    pub bbr_default_model: String,    // default model when none found in body

    // EPP (Endpoint Picker Processor)
    pub epp_enable: bool,
    pub epp_endpoint: Option<String>, // host:port
    pub epp_timeout_ms: u64,
    pub epp_failure_mode_allow: bool, // fail-open
    pub epp_header_name: String,      // default "X-Inference-Upstream"
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self {
            bbr_enable: false,
            bbr_failure_mode_allow: false,
            bbr_header_name: "X-Gateway-Model-Name".to_string(),
            bbr_max_body_size: 10 * 1024 * 1024, // 10MB
            bbr_default_model: "unknown".to_string(),

            epp_enable: false,
            epp_endpoint: None,
            epp_timeout_ms: 200,
            epp_failure_mode_allow: false,
            epp_header_name: "X-Inference-Upstream".to_string(),
        }
    }
}

impl ngx::http::Merge for ModuleConfig {
    fn merge(&mut self, prev: &ModuleConfig) -> Result<(), MergeConfigError> {
        // Inherit enable flags
        if prev.bbr_enable {
            self.bbr_enable = true;
        }
        if prev.epp_enable {
            self.epp_enable = true;
        }

        // Inherit string options if not set
        if self.epp_endpoint.is_none() {
            self.epp_endpoint = prev.epp_endpoint.clone();
        }

        // Inherit numeric with defaults
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
        if self.bbr_default_model.is_empty() {
            self.bbr_default_model = if prev.bbr_default_model.is_empty() {
                "unknown".to_string()
            } else {
                prev.bbr_default_model.clone()
            }
        }
        if self.bbr_max_body_size == 0 {
            self.bbr_max_body_size = if prev.bbr_max_body_size == 0 { 
                10 * 1024 * 1024 
            } else { 
                prev.bbr_max_body_size 
            }; // 10MB default
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

        Ok(())
    }
}

/// Helper functions for configuration parsing
pub fn set_on_off(val: &str) -> Option<bool> {
    if val.eq_ignore_ascii_case("on") {
        Some(true)
    } else if val.eq_ignore_ascii_case("off") {
        Some(false)
    } else {
        None
    }
}

pub fn set_string_opt(target: &mut Option<String>, val: &str) {
    if !val.is_empty() {
        *target = Some(val.to_string());
    }
}

pub fn set_usize(target: &mut usize, val: &str) -> Result<(), ()> {
    match val.parse::<usize>() {
        Ok(v) => {
            *target = v;
            Ok(())
        }
        Err(_) => Err(()),
    }
}

pub fn set_u64(target: &mut u64, val: &str) -> Result<(), ()> {
    match val.parse::<u64>() {
        Ok(v) => {
            *target = v;
            Ok(())
        }
        Err(_) => Err(()),
    }
}