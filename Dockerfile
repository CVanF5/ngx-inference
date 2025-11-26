# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile:
#  - Build stage: compiles the Rust NGINX module (cdylib: libngx_inference.so)
#  - Runtime stage: open-source NGINX image that dynamically loads the module
#
# The module is loaded via:  load_module modules/libngx_inference.so;
# An example nginx.conf with commented directives is provided and copied in.

FROM rust:1.82-slim-bookworm AS builder
WORKDIR /work

# Install build dependencies needed by bindgen/nginx-sys
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential clang libclang-dev pkg-config ca-certificates \
    libpcre2-dev zlib1g-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy source
COPY . .

# Build the dynamic module .so for Linux. The "export-modules" feature exports
# the ngx_modules table so this module can be loaded outside of the NGINX build system.
RUN cargo build --release --features "export-modules,vendored"

# ---------------------------------------------------------------------------------------

# Use the official open-source NGINX runtime (Debian-based, glibc-compatible with our .so)
FROM nginx:stable

# Copy the built dynamic module into the standard modules directory.
COPY --from=builder /work/target/release/libngx_inference.so /etc/nginx/modules/libngx_inference.so

# Copy an example nginx.conf that loads the module and demonstrates the directives.
COPY docker/nginx.conf /etc/nginx/nginx.conf

EXPOSE 80

# Run NGINX in the foreground
CMD ["nginx", "-g", "daemon off;"]
