fn main() {
    // Re-run build if any proto changes
    println!("cargo:rerun-if-changed=proto");

    // Ensure protoc is available using vendored binary to avoid system dependency.
    let protoc_path = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc not found");
    std::env::set_var("PROTOC", &protoc_path);

    // On macOS, allow unresolved NGINX symbols to be resolved at load time.
    // This enables building the dynamic module outside of the NGINX build system.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-undefined,dynamic_lookup");
    }

    // Configure tonic/prost codegen
    let mut cfg = tonic_build::configure()
        // Generate clients and servers for the ext-proc mock server
        .build_client(true)
        .build_server(true)
        // Use prost-types for well-known types
        .compile_well_known_types(true);

    // Map well-known types to prost_types
    cfg = cfg.extern_path(".google.protobuf", "prost_types");

    // Compile the required Envoy ext-proc protos and minimal dependencies from our local vendor dir
    cfg.compile(
        &[
            "proto/envoy/service/ext_proc/v3/external_processor.proto",
            "proto/envoy/extensions/filters/http/ext_proc/v3/processing_mode.proto",
            "proto/envoy/config/core/v3/base.proto",
            "proto/envoy/type/v3/http_status.proto",
        ],
        &["proto"],
    )
    .expect("failed to compile Envoy ext-proc protos");
}
