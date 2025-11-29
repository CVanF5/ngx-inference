# Docker Setup

This directory contains Docker configurations for the ngx-inference module and related services.

## Directory Structure

- `nginx/` - NGINX configurations and Dockerfile for the main inference gateway
- `mock-extproc/` - Mock external processor service implementing EPP (Endpoint Picker Processor) and BBR functionality
- `echo-server/` - Simple Node.js echo server for testing

## Quick Start

### Build and Run with Docker Compose

The docker-compose configuration is located in the `tests/` directory:

```bash
# From the project root
docker-compose -f tests/docker-compose.yml up -d

# Or run from the tests directory
cd tests
docker-compose up -d
```

Access the stack:
- Basic test: `curl -i http://localhost:8081/`
- EPP test: `curl -i http://localhost:8081/epp-test`
- BBR test: `curl -i http://localhost:8081/bbr-test -H 'Content-Type: application/json' --data '{"model":"claude-4","input": "Why is the sky blue"}'`
- BBR & EPP test: `curl -i http://localhost:8081/responses -H 'Content-Type: application/json' --data '{"model":"claude-4","input": "Why is the sky blue"}'`
- Health check: `curl -i http://localhost:8081/health`

### Build Individual Services

```bash
# Build NGINX with ngx-inference module
docker build -f docker/nginx/Dockerfile -t ngx-inference:latest .

# Build mock external processor service
docker build -f docker/mock-extproc/Dockerfile -t extproc-mock:latest .

# Build echo server
docker build -f docker/echo-server/Dockerfile -t echo-server:latest docker/echo-server/
```

## NGINX Container

The `nginx/` directory contains:
- `Dockerfile` - Multi-stage build for NGINX with ngx-inference module
- `nginx-test.conf` - Test configuration for development and testing

### Usage

```bash
docker run -p 8081:80 \
  -v ./docker/nginx/nginx-test.conf:/etc/nginx/nginx.conf:ro \
  ngx-inference:latest
```

## Mock External Processor

The `mock-extproc/` directory contains the mock external processor service that implements both EPP (Endpoint Picker Processor) and BBR (Body Buffer & Rewrite) functionality via gRPC. It uses the `extproc_mock` binary from this repository.

### Usage

```bash
# Run as EPP (Endpoint Picker Processor) on port 9001
docker run -p 9001:9001 \
  -e EPP_UPSTREAM=echo-server:80 \
  extproc-mock:latest

# Run as BBR on port 9000
docker run -p 9000:9000 \
  -e BBR_MODEL=bbr-chosen-model \
  extproc-mock:latest \
  extproc_mock 0.0.0.0:9000
```

## Echo Server

The `echo-server/` directory contains a simple Node.js echo server for testing with a 50MB payload limit:
- `custom-echo-server.js` - Simple Node.js echo server implementation
- `Dockerfile` - Dockerfile for the echo server
- `package.json` - Node.js dependencies

## Environment Variables

### NGINX Container

The NGINX container uses an official NGINX image with the ngx-inference module dynamically loaded. Configuration is provided via volume-mounted nginx.conf files.

### Mock External Processor

- `EPP_UPSTREAM` - Upstream endpoint for EPP routing (default: echo-server:80)
- `BBR_MODEL` - Model identifier for BBR responses (default: bbr-chosen-model)
- `MOCK_ROLE` - Role identifier (e.g., EPP, BBR)

### Echo Server

- `PORT` - Server listening port (default: 80)

## Development

For development and testing, use the docker-compose configuration in the `tests/` directory. The stack includes:

- **nginx**: NGINX with ngx-inference module (exposed on port 8081)
- **mock-epp**: Mock external processor for EPP on port 9001
- **echo-server**: Simple echo server for testing (internal only, accessed via nginx)

All services share the default network allowing DNS resolution by service name.

See `tests/docker-compose.yml` for the complete configuration.

## Troubleshooting

1. **Module loading issues**: Ensure the ngx-inference module is properly compiled and the path in the nginx configuration is correct.

2. **gRPC connection issues**: Verify that the external processor service is running and accessible on the configured endpoint.

3. **Permission issues**: Make sure NGINX has proper permissions to read configuration files and write to log directories.

4. **Build failures**: Check that all required build dependencies are available and the Rust toolchain is properly configured.
