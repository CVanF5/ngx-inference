// Generated gRPC bindings will be included via tonic::include_proto! macros.
// Ensure build.rs runs tonic_build::compile_protos for these packages.

pub mod envoy {
    pub mod service {
        pub mod ext_proc {
            pub mod v3 {
                tonic::include_proto!("envoy.service.ext_proc.v3");
            }
        }
    }

    pub mod extensions {
        pub mod filters {
            pub mod http {
                pub mod ext_proc {
                    pub mod v3 {
                        tonic::include_proto!("envoy.extensions.filters.http.ext_proc.v3");
                    }
                }
            }
        }
    }

    pub mod config {
        pub mod core {
            pub mod v3 {
                tonic::include_proto!("envoy.config.core.v3");
            }
        }
    }

    pub mod r#type {
        pub mod v3 {
            tonic::include_proto!("envoy.r#type.v3");
        }
    }
}
