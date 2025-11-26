# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile:
#  - Build stage: compiles the Rust NGINX module (cdylib: libngx_inference.so)
#  - Runtime stage: open-source NGINX image that dynamically loads the module
#
# The module is loaded via:  load_module modules/libngx_inference.so;
# An example nginx.conf with commented directives is provided and copied in.

FROM rust:1.91-trixie AS builder
WORKDIR /work

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    && apt-get dist-clean

ENV PATH=/root/.cargo/bin:$PATH

# Copy source
COPY . .

# Build the dynamic module .so for Linux. The "export-modules" feature exports
# the ngx_modules table so this module can be loaded outside of the NGINX build system.
RUN cargo build --release --features "export-modules,vendored" --lib

# ---------------------------------------------------------------------------------------

# Use official NGINX 1.28 Debian Bookworm image
FROM nginx:1.28-bookworm
# Ensure standard Debian modules directory exists for custom module
RUN mkdir -p /usr/lib/nginx/modules

# Copy the built dynamic module into the standard modules directory for Debian nginx (/usr/lib/nginx/modules).
COPY --from=builder /work/target/release/libngx_inference.so /usr/lib/nginx/modules/libngx_inference.so

# Copy an example nginx.conf that loads the module and demonstrates the directives.
COPY docker/nginx.conf /etc/nginx/nginx.conf

EXPOSE 80

# Run NGINX in the foreground
CMD ["nginx", "-g", "daemon off;"]
