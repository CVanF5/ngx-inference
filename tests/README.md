# Tests Directory

This directory contains test scripts and utilities for validating the ngx-inference module functionality.

## Docker-Based Tests

### `test_docker_simple.sh`

Main test runner that validates BBR and EPP module configurations using Docker Compose:

- **BBR ON + EPP OFF**: Tests model extraction only
- **BBR OFF + EPP ON**: Tests upstream selection only  
- **BBR ON + EPP ON**: Tests both modules active
- **BBR OFF + EPP OFF**: Tests no processing

Uses dynamic configuration swapping via volume mounts and tests against containerized nginx with existing `docker-compose.yml` setup.

#### Usage

```bash
# Start services and run all configuration tests
./tests/test_docker_simple.sh
```

### `test_large_body.sh`

Tests the BBR (Body-Based Routing) module with various payload sizes:

- **Small body (< 16KB)**: Tests memory buffering
- **Medium body (> 16KB)**: Tests file buffering  
- **Large body (~9MB)**: Tests near BBR 10MB limit
- **Very large body (~12MB)**: Tests BBR limit enforcement

#### Usage

```bash
# Run from project root (requires docker-compose services running)
./tests/test_large_body.sh
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

### `docker-compose.test.yml`

Docker Compose override for test environments:

- Adds test-runner service (optional)
- Provides volume mounts for configuration testing
- Use with: `docker-compose -f docker-compose.yml -f tests/docker-compose.test.yml up`

### `setup_test_env.sh`

Environment setup and validation script:

- Validates required tools (docker-compose, curl, jq) 
- Checks for Docker Compose services availability
- Creates necessary directories
- Provides guidance for service setup

#### Usage

```bash
# Check test environment readiness
./tests/setup_test_env.sh
```

## Test Configurations

The `/tests/configs/` directory contains nginx configurations for each test scenario:

- `nginx-bbr_on_epp_off.conf`: BBR enabled, EPP disabled
- `nginx-bbr_off_epp_on.conf`: BBR disabled, EPP enabled
- `nginx-bbr_on_epp_on.conf`: Both modules enabled
- `nginx-bbr_off_epp_off.conf`: Both modules disabled
- `nginx-original.conf`: Backup of original configuration

## Test Requirements

- Docker and docker-compose
- `curl` and `jq` on the host system
- Existing docker-compose services (nginx, echo-server, mock-epp)

## Quick Start

```bash
# Start Docker services
docker-compose up -d

# Run configuration tests
./tests/test_docker_simple.sh

# Run BBR functionality tests
./tests/test_large_body.sh
```

## Notes

- Tests use dynamic nginx configuration reloading via `nginx -s reload`
- Each test restores the original configuration when complete
- Tests include colored output for better readability
- All tests work against the containerized environment on `localhost:8081`
- Mock gRPC EPP service on `mock-epp:9001` provides realistic upstream selection
- Echo server provides request inspection and header validation