# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile to build and run the mock Envoy ExternalProcessor server
# used by the ngx-inference module tests. This compiles the `extproc_mock` binary
# from this repository and packages it into a minimal Debian runtime image.

FROM rust:1.91-trixie AS builder
WORKDIR /work

# Dependencies required when building the crate with the `vendored` feature.
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    && apt-get dist-clean


# Copy source
COPY . .

# Build the mock server binary with vendored + extproc-mock features
# - vendored: satisfy ngx/nginx-sys build requirements
# - extproc-mock: enable serde/serde_json used for parsing JSON request bodies
RUN cargo build --release --features "vendored,extproc-mock" --bin extproc_mock

# ------------------------------------------------------------------------------------------------

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && apt-get dist-clean

COPY --from=builder /work/target/release/extproc_mock /usr/local/bin/extproc_mock

# Default environment values; can be overridden via docker-compose
ENV EPP_UPSTREAM=echo-server:80 \
    BBR_MODEL=bbr-chosen-model

EXPOSE 9000 9001

# By default, listen on 9001 (EPP). For BBR instance, override the command in compose to 9000.
CMD ["extproc_mock", "0.0.0.0:9001"]
