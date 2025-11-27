# Docker Setup

This directory contains Docker configurations for the ngx-inference module and related services.

## Directory Structure

- `nginx/` - NGINX configurations and Dockerfile for the main inference gateway
- `extproc/` - External processor service for EPP (Endpoint Picker Processor)
- `examples/` - Example services and configurations for testing

## Quick Start

### Build and Run with Docker Compose

```bash
# From the project root
docker-compose up -d
```

### Build Individual Services

```bash
# Build NGINX with ngx-inference module
docker build -f docker/nginx/Dockerfile -t ngx-inference:latest .

# Build external processor service
docker build -f docker/extproc/Dockerfile -t extproc-service:latest .

# Build example echo server
docker build -f docker/examples/custom-echo.Dockerfile -t echo-server:latest docker/examples/
```

## NGINX Container

The `nginx/` directory contains:
- `Dockerfile` - Multi-stage build for NGINX with ngx-inference module
- `nginx-test.conf` - Test configuration for development and testing

### Usage

```bash
docker run -p 80:80 \
  -v ./examples/basic-config/nginx.conf:/etc/nginx/nginx.conf:ro \
  ngx-inference:latest
```

## External Processor

The `extproc/` directory contains the external processor service that implements the EPP (Endpoint Picker Processor) functionality via gRPC.

### Usage

```bash
docker run -p 9001:9001 extproc-service:latest
```

## Examples

The `examples/` directory contains sample services for testing:
- `custom-echo-server.js` - Simple Node.js echo server for testing
- `custom-echo.Dockerfile` - Dockerfile for the echo server

## Environment Variables

### NGINX Container

- `NGINX_WORKER_PROCESSES` - Number of worker processes (default: auto)
- `NGINX_WORKER_CONNECTIONS` - Worker connections (default: 1024)

### External Processor

- `GRPC_PORT` - gRPC server port (default: 9001)
- `LOG_LEVEL` - Logging level (default: info)

## Development

For development and testing, you can use the provided docker-compose configuration:

```yaml
# In your docker-compose.yml
version: '3.8'
services:
  nginx:
    build:
      context: .
      dockerfile: docker/nginx/Dockerfile
    ports:
      - "80:80"
    volumes:
      - ./examples/basic-config/nginx.conf:/etc/nginx/nginx.conf:ro
    depends_on:
      - extproc
      - echo-server

  extproc:
    build:
      context: .
      dockerfile: docker/extproc/Dockerfile
    environment:
      - GRPC_PORT=9001

  echo-server:
    build:
      context: docker/examples
      dockerfile: custom-echo.Dockerfile
```

## Troubleshooting

1. **Module loading issues**: Ensure the ngx-inference module is properly compiled and the path in the nginx configuration is correct.

2. **gRPC connection issues**: Verify that the external processor service is running and accessible on the configured endpoint.

3. **Permission issues**: Make sure NGINX has proper permissions to read configuration files and write to log directories.

4. **Build failures**: Check that all required build dependencies are available and the Rust toolchain is properly configured.