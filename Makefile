# Makefile for ngx-inference

# Configuration variables
DOCKER_COMPOSE_MAIN = tests/docker-compose.yml
CARGO_FEATURES = export-modules,vendored
PID_FILE = /tmp/nginx-ngx-inference.pid
KIND_CLUSTER_NAME = ngx-inference-test

.PHONY: help setup-local setup-docker setup-kind start-local start-docker start-kind test-local test-docker test-kind stop clean lint build check

# Default target
help:
	@echo "NGX-Inference Makefile"
	@echo ""
	@echo "QUICK START:"
	@echo "  make start-local    Setup, build, and start local services"
	@echo "  make start-docker   Setup and start full Docker stack"
	@echo "  make start-kind     Setup, create kind cluster, and deploy reference EPP"
	@echo ""
	@echo "SETUP (check/install dependencies - automatically run by start targets):"
	@echo "  setup-local    Validate local development environment"
	@echo "  setup-docker   Validate Docker development environment"
	@echo "  setup-kind     Validate KIND development environment"
	@echo ""
	@echo "TEST (run automated tests):"
	@echo "  test-local     Run local tests"
	@echo "  test-docker    Run Docker-based tests"
	@echo "  test-kind      Run tests against TLS-enabled reference EPP in kind cluster"
	@echo ""
	@echo "UTILITY:"
	@echo "  stop           Stop all services (local, Docker, and kind)"
	@echo "  clean          Stop services and clean all build artifacts"
	@echo "  lint           Run Rust linting and formatting checks"
	@echo "  build          Build inference module and mock server"
	@echo "  check          Quick compilation check without building"
	@echo ""
	@echo "EXAMPLES:"
	@echo "  make start-local && make test-local    # Local workflow"
	@echo "  make start-docker && make test-docker  # Docker workflow"
	@echo "  make start-kind && make test-kind      # Kind workflow"
	@echo "  make stop                              # Stop everything"

# ============================================================================
# SETUP TARGETS - Check and validate dependencies
# ============================================================================

setup-local:
	@echo "==> Setting up local development environment..."
	./tests/setup-local-dev.sh --local
	@echo "✅ Local setup complete. Run 'make start-local' to start services."

setup-docker:
	@echo "==> Setting up Docker development environment..."
	./tests/setup-local-dev.sh --docker
	@echo "✅ Docker setup complete. Run 'make start-docker' to start services."

setup-kind:
	@echo "==> Setting up KIND development environment..."
	./tests/setup-local-dev.sh --kind
	@echo "✅ Kind setup complete. Run 'make start-kind' to create cluster and deploy."

# ============================================================================
# START TARGETS - Build and run services
# ============================================================================

start-local: setup-local build
	@echo "==> Starting local services..."
	@echo "Starting mock external processor..."
	@-kill $$(cat /tmp/extproc_mock.pid 2>/dev/null) 2>/dev/null || true
	@rm -f /tmp/extproc_mock.pid
	@(EPP_UPSTREAM=localhost:8080 MOCK_ROLE=EPP ./target/debug/extproc_mock 0.0.0.0:9001 >/dev/null 2>&1 & echo $$! > /tmp/extproc_mock.pid) || true
	@sleep 1
	@echo "Starting echo server..."
	@-kill $$(cat /tmp/echo-server.pid 2>/dev/null) 2>/dev/null || true
	@rm -f /tmp/echo-server.pid
	@cd docker/echo-server && [ ! -f package.json ] && npm init -y >/dev/null 2>&1 || true
	@cd docker/echo-server && [ ! -d node_modules ] && npm install express >/dev/null 2>&1 || true
	@(cd docker/echo-server && PORT=8080 node custom-echo-server.js >/dev/null 2>&1 & echo $$! > /tmp/echo-server.pid) || true
	@sleep 1
	@echo "✅ Local services started. Run 'make test-local' to run tests."

start-docker: setup-docker
	@echo "==> Starting Docker services..."
	docker compose -f $(DOCKER_COMPOSE_MAIN) up --build -d
	@echo "✅ Docker services started. Run 'make test-docker' to run tests."

start-kind: setup-kind
	@echo "==> Setting up kind cluster and deploying components..."
	./tests/kind-ngf/scripts/setup.sh
	@echo "✅ Kind cluster ready. Run 'make test-kind' to run tests."

# ============================================================================
# TEST TARGETS - Run automated tests
# ============================================================================

test-local:
	@echo "==> Running local tests..."
	@echo "Building module for NGINX version: $$(nginx -v 2>&1 | sed 's|nginx version: nginx/||')"
	./tests/test-config.sh
	@echo "✅ Local tests complete."

test-docker:
	@echo "==> Running Docker tests..."
	@echo "Building module for NGINX version: $$(grep 'FROM nginx:' docker/nginx/Dockerfile | head -1 | sed 's/.*nginx://' | sed 's/-.*//')"
	DOCKER_ENVIRONMENT=main ./tests/test-config.sh
	@echo "✅ Docker tests complete."

test-kind:
	@echo "==> Running tests against reference EPP in kind cluster..."
	./tests/kind-ngf/scripts/test-kind.sh
	@echo "✅ Kind tests complete."

# ============================================================================
# UTILITY TARGETS
# ============================================================================

stop:
	@echo "==> Stopping all services..."
	@# Stop Docker services
	@docker compose -f $(DOCKER_COMPOSE_MAIN) down --remove-orphans 2>/dev/null || true
	@# Stop kind cluster
	@kind delete cluster --name $(KIND_CLUSTER_NAME) 2>/dev/null || true
ifndef GITHUB_ACTIONS
	@# Stop nginx if running
	@-[ -f $(PID_FILE) ] && kill -TERM $$(cat $(PID_FILE)) 2>/dev/null || true
	@-[ -f $(PID_FILE) ] && rm -f $(PID_FILE) 2>/dev/null || true
	@# Stop backend services using PID files
	@-kill $$(cat /tmp/echo-server.pid 2>/dev/null) 2>/dev/null || true
	@-kill $$(cat /tmp/extproc_mock.pid 2>/dev/null) 2>/dev/null || true
	@rm -f /tmp/extproc_mock.pid /tmp/echo-server.pid 2>/dev/null || true
	@# Fallback: Stop processes by name/port (in case PID files are missing)
	@-pkill -f "custom-echo-server.js" 2>/dev/null || true
	@-pkill -f "extproc_mock" 2>/dev/null || true
endif
	@echo "✅ All services stopped."

clean: stop
	@echo "==> Cleaning build artifacts..."
	@# Clean build artifacts
	cargo clean
	@# Clean temp files
	rm -f /tmp/nginx-ngx-inference-*.log /tmp/nginx-ngx-inference-test.conf
	@# Remove nginx temp directories
	rm -rf /tmp/nginx_client_body_temp /tmp/nginx_proxy_temp /tmp/nginx_fastcgi_temp /tmp/nginx_scgi_temp /tmp/nginx_uwsgi_temp
	@# Clean echo server node_modules
	rm -rf docker/echo-server/node_modules docker/echo-server/package-lock.json docker/echo-server/package.json
	@echo "✅ Cleanup complete."

lint:
	@echo "==> Running Rust linting and formatting checks..."
	@echo "Checking code formatting..."
	@cargo fmt --all -- --check || (echo "❌ Code formatting issues found. Run 'cargo fmt --all' to fix." && exit 1)
	@echo "Running Clippy linter..."
	@cargo clippy --all-targets --all-features -- -D warnings -A clippy::doc-lazy-continuation -A clippy::enum-variant-names || (echo "❌ Clippy issues found." && exit 1)
	@echo "Checking for whitespace issues using git..."
	@if git diff --check 2>/dev/null; then \
		echo "✅ No whitespace issues found by git"; \
	else \
		echo "❌ Whitespace issues found. See above for details."; \
		echo "Run 'git diff --check' to see all whitespace issues."; \
		exit 1; \
	fi
	@echo "Checking for trailing whitespace in source files..."
	@if find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -l '[[:space:]]$$' 2>/dev/null | head -20 | grep -q .; then \
		echo "❌ Trailing whitespace found in source files:"; \
		find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -Hn '[[:space:]]$$' 2>/dev/null | head -20; \
		echo "Run the following to fix: find src/ tests/ \\( -name '*.rs' -o -name '*.sh' \\) -exec sed -i '' 's/[[:space:]]*$$//' {} \\;"; \
		exit 1; \
	fi
	@echo "Checking for tabs instead of spaces..."
	@if find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -l $$'\t' 2>/dev/null | head -10 | grep -q .; then \
		echo "❌ Tab characters found in source files:"; \
		find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -Hn $$'\t' 2>/dev/null | head -10; \
		echo "Please use spaces for indentation."; \
		exit 1; \
	fi
	@echo "Checking for Windows line endings..."
	@if find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -l $$'\r' 2>/dev/null | grep -q .; then \
		echo "❌ Windows line endings (CRLF) found in source files:"; \
		find src/ tests/ -name '*.rs' -o -name '*.sh' | xargs grep -l $$'\r' 2>/dev/null; \
		echo "Run the following to fix: find src/ tests/ \\( -name '*.rs' -o -name '*.sh' \\) -exec dos2unix {} \\;"; \
		exit 1; \
	fi
	@echo "✅ All linting checks passed!"

# ============================================================================
# BUILD TARGETS
# ============================================================================

build:
	@echo "==> Building ngx-inference module and mock server..."
	NGX_VERSION=$$(nginx -v 2>&1 | sed 's|nginx version: nginx/||') \
	NGX_NO_SIGNATURE_CHECK=1 \
	cargo build --features "$(CARGO_FEATURES),extproc-mock,vendored" --lib --bin extproc_mock
	@echo "✅ Build complete."

check:
	@echo "==> Checking code compilation..."
	cargo check --features "$(CARGO_FEATURES),extproc-mock,vendored" --lib --bin extproc_mock
	@echo "✅ Compilation check passed."

# ============================================================================
# LEGACY ALIASES (for compatibility)
# ============================================================================

.PHONY: build-inference build-mock build-check start test test-kind-setup test-kind-cleanup generate-config

build-inference:
	@echo "==> Building ngx-inference module..."
	NGX_VERSION=$$(nginx -v 2>&1 | sed 's|nginx version: nginx/||') \
	NGX_NO_SIGNATURE_CHECK=1 \
	cargo build --features "$(CARGO_FEATURES)" --lib

build-mock:
	@echo "==> Building mock server..."
	cargo build --bin extproc_mock --features "extproc-mock,vendored"

build-check:
	@# Quick existence check first
	@if [ ! -f target/debug/libngx_inference.so ] && [ ! -f target/debug/libngx_inference.dylib ] || [ ! -f target/debug/extproc_mock ]; then \
		echo "Building missing binaries..."; \
		$(MAKE) build; \
	else \
		echo "Checking if rebuild needed..."; \
		NGX_VERSION=$$(nginx -v 2>&1 | sed 's|nginx version: nginx/||') \
		NGX_NO_SIGNATURE_CHECK=1 \
		cargo build --features "$(CARGO_FEATURES),extproc-mock,vendored" --lib --bin extproc_mock; \
	fi

start: start-docker

test:
	@echo "Please specify test environment: make test-local, make test-docker, or make test-kind"
	@exit 1

generate-config:
	@[ -n "$(OUTPUT)" ] || (echo "Error: OUTPUT required. Usage: make generate-config OUTPUT=/path ENV=local|docker TEST=scenario"; exit 1)
	@ENV_ARG=$${ENV:-local}; \
	[ -n "$(TEST)" ] && \
		./tests/generate-config.sh -e $$ENV_ARG -o $(OUTPUT) -s $(TEST) || \
		./tests/generate-config.sh -e $$ENV_ARG -o $(OUTPUT)
