# Tests Directory

This directory contains test scripts and utilities for validating the ngx-inference module functionality.

## Test Scripts

### `test_config.sh`

Main test runner that validates BBR (Body-Based Routing) and EPP (External Processing Pipeline) module configurations. This script supports both local nginx testing and Docker-based testing:

**Configuration Test Matrix:**
- **BBR ON + EPP OFF**: Tests model extraction only
- **BBR OFF + EPP ON**: Tests upstream selection only
- **BBR ON + EPP ON**: Tests both modules active
- **BBR OFF + EPP OFF**: Tests no processing (baseline)

**Execution Modes:**
- **Local Mode**: Uses locally compiled nginx module with local backend services
- **Docker Mode**: Uses containerized environment via `docker-compose.yml`

#### Usage

```bash
# Local nginx testing (default)
./tests/test_config.sh

# Docker-based testing (set via environment variable)
DOCKER_ENVIRONMENT=main ./tests/test_config.sh

# Or use Makefile targets
make test-local    # Local nginx testing
make test-docker   # Docker-based testing
```

### `test_large_body.sh`

Tests the BBR (Body-Based Routing) module with various payload sizes to validate memory and file buffering behavior:

- **Small body (< 16KB)**: Tests memory buffering
- **Medium body (> 16KB)**: Tests file buffering
- **Large body (~9MB)**: Tests near BBR 10MB limit
- **Very large body (~12MB)**: Tests BBR limit enforcement

#### Usage

```bash
# Run from project root (requires services to be running)
./tests/test_large_body.sh

# Or run directly (requires services to be running)
# ./tests/test_large_body.sh
```

#### Test Scenarios

1. **Memory Buffered**: Small JSON payload processed in memory
2. **File Buffered**: Medium payload that exceeds `client_body_buffer_size` and gets written to temporary files
3. **Large Payload**: ~9MB payload testing BBR module's ability to handle large file-backed requests
4. **Oversized Payload**: ~12MB payload that should be rejected by BBR's 10MB limit

#### Expected Results

- Small/Medium/Large payloads: ✅ Model name extracted successfully
- Oversized payloads: ✅ Rejected with appropriate error (502 Bad Gateway or 413 Payload Too Large)

## Test Infrastructure

### `docker-compose.yml`

Main Docker Compose configuration providing the test environment:

- **nginx**: Main web server with ngx-inference module
- **echo-server**: Node.js service for request inspection and header validation
- **mock-extproc**: Mock gRPC External Processing service for EPP module testing

### `setup-local-dev.sh`

Comprehensive development environment setup and validation script:

- Supports both local nginx development and Docker-based testing
- Validates required tools (nginx, node, cargo for local; docker, docker-compose for Docker)
- Checks for necessary dependencies and provides installation guidance
- Creates necessary directories
- Provides guidance for development workflow

#### Usage

```bash
# Setup for local development (default)
./tests/setup-local-dev.sh
./tests/setup-local-dev.sh --local

# Setup for Docker-based testing
./tests/setup-local-dev.sh --docker

# Show help
./tests/setup-local-dev.sh --help
```

### `generate-config.sh`

Utility script for generating nginx configuration files from templates with proper module paths and resolver settings.

### `configs/nginx-base.conf`

Base nginx configuration template used by both local and Docker testing environments. Uses placeholders that are replaced during test execution:

- `MODULE_PATH_PLACEHOLDER`: Replaced with appropriate module path for local or Docker environment
- `TEST_SERVER_CONFIG_PLACEHOLDER`: Replaced with specific server configuration from `/tests/configs/`
- `RESOLVER_PLACEHOLDER`: Replaced with system DNS resolver

This template approach allows consistent nginx configuration across environments while supporting different test scenarios through modular server configurations.

## Test Configurations

The `/tests/configs/` directory contains server configuration files for each test scenario:

**Configuration Files:**
- `bbr_on_epp_off.conf`: BBR enabled, EPP disabled
- `bbr_off_epp_on.conf`: BBR disabled, EPP enabled
- `bbr_on_epp_on.conf`: Both modules enabled
- `bbr_off_epp_off.conf`: Both modules disabled

These server configuration files are combined with `nginx-base.conf` to create complete nginx configurations for testing different module combinations.

## Manual Testing Examples

### EPP (Endpoint Picker Processor) Testing

```bash
# Headers-only request - EPP selects upstream based on headers
curl -i http://localhost:8081/epp-test \
  -H "Content-Type: application/json" \
  -H "X-Request-Id: test-epp-123"
```

### BBR (Body-Based Routing) Testing

```bash
# Request with JSON body - BBR extracts model name from "model" field
curl -i http://localhost:8081/bbr-test \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4", "prompt": "Hello world", "temperature": 0.7}'

# JSON without "model" field - BBR uses configured fallback
curl -i http://localhost:8081/bbr-test \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Hello world", "temperature": 0.7}'
```

### Combined BBR + EPP Pipeline Testing

```bash
# JSON with model field - both BBR and EPP process the request
curl -i http://localhost:8081/responses \
  -H "Content-Type: application/json" \
  -H "X-Client-Id: mobile-app" \
  -d '{"model": "claude-3", "messages": [{"role": "user", "content": "Hello"}]}'
```

### Expected Response Headers

When testing, you should see these headers in the echo server response indicating successful processing:

- `x-gateway-model-name` - Set by BBR based on JSON "model" field
- `x-inference-upstream` - Set by EPP for upstream selection
- Original request headers forwarded to the upstream

### Test Environment Components

The Docker Compose stack includes:

- **nginx** (port 8081) - NGINX with ngx-inference module
- **mock-extproc** (internal port 9001) - Mock EPP external processor
- **echo-server** (internal port 8080) - Target upstream that echoes request details

## Available Make Targets

- `make setup-local` - Setup local development environment
- `make setup-docker` - Setup Docker-based testing environment
- `make build` - Build the ngx-inference module
- `make start-local` - Start local nginx with compiled module
- `make start-dev` - Start Docker services for development
- `make test` - Run tests (local by default, Docker if TEST_ENV=docker)
- `make test-local` - Run configuration tests with local nginx
- `make test-docker` - Run configuration tests with Docker

- `make stop` - Stop running services
- `make clean` - Clean build artifacts and temporary files

## Troubleshooting

### Common Issues

- **502 Bad Gateway:** Check if external processors are running and reachable
  - Enable fail-open mode: `inference_*_failure_mode_allow on`
  - Verify endpoints: `inference_bbr_endpoint` and `inference_epp_endpoint`

- **Headers not set:**
  - Check external processor logs for JSON parsing errors
  - Verify Content-Type is `application/json` for BBR tests
  - Ensure JSON contains valid "model" field for BBR

- **DNS resolution errors:**
  - In Docker: Use service names (`mock-extproc:9001`)
  - Local testing: Use `localhost` or `127.0.0.1`
  - Check NGINX resolver configuration

- **Module not loading:**
  - Verify dynamic library path in `load_module` directive
  - Check NGINX error log for loading errors
  - Ensure library was built with `export-modules` feature

### Debug Tips

- Use `error_log` and debug logging to verify module activation
- The access-phase handler logs `ngx-inference: bbr_enable=<..> epp_enable=<..>` per request
- Check if EPP endpoints are reachable; toggle `*_failure_mode_allow on` to fail-open during testing
- Ensure EPP implementation returns header mutation for upstream endpoint selection
- BBR processes JSON directly - ensure request bodies contain valid JSON with a "model" field

## Test Requirements

### Docker Environment
- Docker and docker-compose
- `curl` and `jq` on the host system

### Local Environment
- nginx with dynamic module support
- Node.js (for local echo server)
- Rust/Cargo toolchain
- `curl`, `jq`, and `nc` (netcat)

## Quick Start

### Docker Environment (Recommended)
```bash
# Build and start Docker services
make start-dev

# Run configuration tests
make test-docker

# Run BBR functionality tests directly
# ./tests/test_large_body.sh
```

### Local Development Environment
```bash
# Setup local development environment
make setup-local

# Start local nginx with compiled module
make start-local

# Run configuration tests
make test-local

# Run BBR functionality tests directly
# ./tests/test_large_body.sh
```

## Notes

- **Configuration Testing**: `test_config.sh` uses dynamic nginx configuration reloading via `nginx -s reload`
- **State Management**: Each test restores the original configuration when complete
- **Output**: Tests include colored output for better readability
- **Networking**: All tests work against nginx on `localhost:8081` (both Docker and local)
- **Mock Services**: Mock gRPC EPP service on `mock-extproc:9001` provides realistic upstream selection
- **Echo Server**: Provides request inspection and header validation at `echo-server:3000`