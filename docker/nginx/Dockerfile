# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile:
#  - Build stage: compiles the Rust NGINX module (cdylib: libngx_inference.so)
#  - Runtime stage: open-source NGINX image that dynamically loads the module
#
# The module is loaded via:  load_module /usr/lib/nginx/modules/libngx_inference.so;

FROM rust:1.91-alpine3.22 AS builder
WORKDIR /work

# Install build dependencies for Alpine (NGINX requirements)
RUN apk add --no-cache \
    clang-dev \
    pcre2-dev \
    openssl-dev \
    make \
    && rm -rf /var/cache/apk/*

ENV PATH=/root/.cargo/bin:$PATH

# Copy source
COPY . .

# Build the dynamic module .so for Linux. The "export-modules" feature exports
# the ngx_modules table so this module can be loaded outside of the NGINX build system.
ENV RUSTFLAGS="-C target-feature=-crt-static"
ENV NGX_VERSION="1.29.3"
RUN cargo build --release --features "export-modules,vendored" --lib

# ---------------------------------------------------------------------------------------

# Use official NGINX 1.29 Alpine image
FROM nginx:1.29.3-alpine

# Ensure standard modules directory exists and NGINX has permission to load
RUN mkdir -p /usr/lib/nginx/modules

# Copy the built dynamic module into the standard modules directory for Debian nginx (/usr/lib/nginx/modules).
COPY --from=builder /work/target/release/libngx_inference.so /usr/lib/nginx/modules/libngx_inference.so

EXPOSE 80

# Run NGINX in the foreground
CMD ["nginx", "-g", "daemon off;"]
