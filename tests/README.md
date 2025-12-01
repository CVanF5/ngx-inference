# Tests Directory

This directory contains test scripts and utilities for validating the ngx-inference module functionality.

## Test Scripts

### `test-config.sh`

Main test runner that validates BBR (Body-Based Routing) and EPP (External Processing Pipeline) module configurations. This script supports three testing environments:

**Configuration Test Matrix:**
- **BBR ON + EPP OFF**: Tests model extraction only
- **BBR OFF + EPP ON**: Tests upstream selection only  
- **BBR ON + EPP ON**: Tests both modules active
- **BBR OFF + EPP OFF**: Tests no processing (baseline)

**Execution Modes:**
- **Local Mode**: Uses locally compiled nginx module with local backend services
- **Docker Mode**: Uses containerized environment via `docker-compose.yml`
- **Kind Mode**: Uses Kubernetes-in-Docker with reference EPP implementation

#### Usage

```bash
# Use Makefile targets (recommended)
make test-local    # Local nginx testing
make test-docker   # Docker-based testing
make test-kind     # Kubernetes testing in Kind

# Or run directly
./tests/test-config.sh                    # Local mode
DOCKER_ENVIRONMENT=main ./tests/test-config.sh  # Docker mode
```

## Test Infrastructure

### `docker-compose.yml`

Main Docker Compose configuration providing the test environment:

- **nginx**: Main web server with ngx-inference module
- **echo-server**: Node.js service for request inspection and header validation
- **mock-extproc**: Mock gRPC External Processing service for EPP module testing

### Kind Testing Environment (`kind-ngf/`)

Kubernetes-in-Docker testing infrastructure with:

- **Kind cluster**: Lightweight Kubernetes cluster for testing
- **Reference EPP**: Production-ready External Processing implementation
- **TLS Support**: Tests TLS-enabled gRPC communication
- **Real-world scenarios**: Tests against actual Kubernetes deployment patterns

#### Kind Directory Structure
- `cluster/`: Kubernetes cluster configuration
- `manifests/`: Deployment manifests for nginx and EPP services
- `scripts/`: Setup and testing automation scripts

### `setup-local-dev.sh`

Comprehensive development environment setup and validation script supporting all three testing environments:

- **Local Development**: Validates nginx, Node.js, Rust toolchain
- **Docker Environment**: Validates Docker and docker-compose
- **Kind Environment**: Validates kubectl, kind, and Kubernetes tools
- Provides installation guidance for missing dependencies
- Creates necessary directories and validates configurations

#### Usage

```bash
# Setup for local development (default)
./tests/setup-local-dev.sh
./tests/setup-local-dev.sh --local

# Setup for Docker-based testing
./tests/setup-local-dev.sh --docker

# Setup for Kind/Kubernetes testing
./tests/setup-local-dev.sh --kind

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

The Makefile provides comprehensive workflow automation for all testing environments:

### Quick Start Targets
- `make start-local` - Setup, build, and start local development environment
- `make start-docker` - Setup and start full Docker stack  
- `make start-kind` - Setup, create Kind cluster, and deploy reference EPP

### Setup Targets (automatically run by start targets)
- `make setup-local` - Validate local development environment
- `make setup-docker` - Validate Docker development environment  
- `make setup-kind` - Validate Kind development environment

### Test Targets
- `make test-local` - Run configuration tests with local nginx
- `make test-docker` - Run configuration tests with Docker
- `make test-kind` - Run tests against TLS-enabled reference EPP in Kind cluster

### Build and Utility Targets
- `make build` - Build the ngx-inference module and mock server
- `make check` - Quick compilation check without full build
- `make lint` - Run Rust linting and formatting checks
- `make stop` - Stop all services (local, Docker, and Kind)
- `make clean` - Clean build artifacts and temporary files

### Example Workflows
```bash
# Local development workflow
make start-local && make test-local

# Docker workflow  
make start-docker && make test-docker

# Kind/Kubernetes workflow
make start-kind && make test-kind

# Stop everything
make stop
```

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

### Local Environment
- nginx with dynamic module support
- Node.js (for echo server)
- Rust/Cargo toolchain
- `curl`, `jq`, and basic Unix utilities

### Docker Environment
- Docker and docker-compose
- `curl` and `jq` on the host system

### Kind Environment  
- Docker
- kubectl
- kind (Kubernetes in Docker)
- `curl` and `jq` on the host system

## Quick Start

### Docker Environment (Recommended)
```bash
# Setup, build and start Docker services
make start-docker

# Run configuration tests
make test-docker
```

### Local Development Environment  
```bash
# Setup, build and start local services
make start-local

# Run configuration tests
make test-local
```

### Kind/Kubernetes Environment
```bash
# Setup Kind cluster and deploy reference EPP
make start-kind

# Run tests against TLS-enabled EPP
make test-kind
```

## Testing Environments Comparison

| Environment | Use Case | Benefits | Requirements |
|-------------|----------|----------|--------------|
| **Local** | Development, debugging | Fast iteration, easy debugging | nginx, Node.js, Rust |
| **Docker** | Integration testing | Consistent environment, easy setup | Docker, docker-compose |
| **Kind** | Production validation | Real Kubernetes, TLS, production EPP | kubectl, kind, Docker |

## Notes

- **Three Testing Environments**: Local development, Docker containerization, and Kind/Kubernetes for production validation
- **Configuration Testing**: `test-config.sh` uses dynamic nginx configuration reloading via `nginx -s reload`
- **State Management**: Each test restores the original configuration when complete
- **Output**: Tests include colored output for better readability
- **Networking**: All tests work against nginx on `localhost:8081` (local and Docker) or cluster IP (Kind)
- **Mock Services**: Mock gRPC EPP service provides realistic upstream selection in local/Docker environments
- **Echo Server**: Provides request inspection and header validation
- **Production EPP**: Kind environment uses a reference External Processing implementation with TLS support