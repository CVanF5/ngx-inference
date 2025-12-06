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
RUN apk add \
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

# Install ca-certificates package for certificate management
RUN apk add --no-cache ca-certificates

# Ensure standard modules directory exists and NGINX has permission to load
RUN mkdir -p /usr/lib/nginx/modules

# Copy the built dynamic module into the standard modules directory for Debian nginx (/usr/lib/nginx/modules).
COPY --from=builder /work/target/release/libngx_inference.so /usr/lib/nginx/modules/libngx_inference.so

# Create entrypoint script to handle certificate installation at runtime
RUN cat > /docker-entrypoint.d/10-install-epp-cert.sh << 'EOF'
#!/bin/sh
set -e

# Check if EPP certificate is available and install it to CA trust store
if [ -f "/etc/epp-tls/tls.crt" ]; then
    echo "Installing EPP TLS certificate to CA trust store..."
    cp /etc/epp-tls/tls.crt /usr/local/share/ca-certificates/epp-tls.crt
    update-ca-certificates
    echo "EPP certificate installed successfully"
else
    echo "No EPP certificate found at /etc/epp-tls/tls.crt - skipping CA installation"
fi
EOF

# Make the script executable
RUN chmod +x /docker-entrypoint.d/10-install-epp-cert.sh

EXPOSE 80

# Run NGINX in the foreground
CMD ["nginx", "-g", "daemon off;"]
