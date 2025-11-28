use crate::modules::{bbr::get_header_in, config::ModuleConfig};
use ngx::http;

/// EPP (Endpoint Picker Processor) processor
/// Communicates with external gRPC services to determine upstream routing
pub struct EppProcessor;

impl EppProcessor {
    /// Process EPP for a request if enabled
    pub fn process_request(
        request: &mut http::Request,
        conf: &ModuleConfig,
    ) -> Result<(), &'static str> {
        if !conf.epp_enable {
            return Ok(());
        }

        Self::pick_upstream(request, conf)
    }

    fn pick_upstream(request: &mut http::Request, conf: &ModuleConfig) -> Result<(), &'static str> {
        // If EPP endpoint is not configured, skip.
        let endpoint = match &conf.epp_endpoint {
            Some(e) if !e.is_empty() => e.as_str(),
            _ => return Ok(()),
        };

        let upstream_header = if conf.epp_header_name.is_empty() {
            "X-Inference-Upstream".to_string()
        } else {
            conf.epp_header_name.clone()
        };
        let upstream_header_str = upstream_header.as_str();

        // If upstream already set (e.g., previous stage), skip.
        if get_header_in(request, upstream_header_str).is_some() {
            return Ok(());
        }

        // Call gRPC client: forward incoming headers to EPP for richer context;
        // headers-only request to pick upstream.
        let mut hdrs: Vec<(String, String)> = Vec::new();
        for (name, value) in request.headers_in_iterator() {
            if let (Ok(n), Ok(v)) = (name.to_str(), value.to_str()) {
                hdrs.push((n.to_string(), v.to_string()));
            }
        }

        match crate::grpc::epp_headers_blocking(
            endpoint,
            conf.epp_timeout_ms,
            upstream_header_str,
            hdrs,
        ) {
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
}
