# Makefile for ngx-inference

# Configuration variables
TEST_ENV ?= local
DOCKER_COMPOSE_MAIN = tests/docker-compose.yml
CARGO_FEATURES = export-modules,vendored
PID_FILE = /tmp/nginx-ngx-inference.pid

.PHONY: build build-inference build-mock build-check check clean start start-local start-docker validate-local setup-local setup-docker test test-docker test-local generate-config stop deploy lint help

# Build targets
build:
	@echo "Building ngx-inference module and mock server..."
	NGX_VERSION=$$(nginx -v 2>&1 | sed 's|nginx version: nginx/||') \
	NGX_NO_SIGNATURE_CHECK=1 \
	cargo build --features "$(CARGO_FEATURES),extproc-mock,vendored" --lib --bin extproc_mock

# Quick check if code compiles without building
check:
	@echo "Checking code compilation..."
	cargo check --features "$(CARGO_FEATURES),extproc-mock,vendored" --lib --bin extproc_mock

# Build only if binaries don't exist or sources are newer
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

build-inference:
	@echo "Building ngx-inference module..."
	NGX_VERSION=$$(nginx -v 2>&1 | sed 's|nginx version: nginx/||') \
	NGX_NO_SIGNATURE_CHECK=1 \
	cargo build --features "$(CARGO_FEATURES)" --lib

build-mock:
	@echo "Building mock server..."
	cargo build --bin extproc_mock --features "extproc-mock,vendored"

# Linting targets
lint:
	@echo "Running Rust linting and formatting checks..."
	@echo "Checking code formatting..."
	@cargo fmt --all -- --check || (echo "❌ Code formatting issues found. Run 'cargo fmt --all' to fix." && exit 1)
	@echo "Running Clippy linter..."
	@cargo clippy --all-targets --all-features -- -D warnings -A clippy::doc-lazy-continuation -A clippy::enum-variant-names || (echo "❌ Clippy issues found." && exit 1)
	@echo "Checking for trailing whitespace..."
	@if find src/ -name '*.rs' -exec grep -l '[[:space:]]$$' {} \; 2>/dev/null | head -20 | grep -q .; then \
		echo "❌ Trailing whitespace found in source files:"; \
		find src/ -name '*.rs' -exec grep -Hn '[[:space:]]$$' {} \; 2>/dev/null | head -20; \
		echo "Run the following to fix: find src/ -name '*.rs' -exec sed -i '' 's/[[:space:]]*$$//' {} \\;"; \
		exit 1; \
	fi
	@echo "Checking for tabs instead of spaces..."
	@if find src/ -name '*.rs' -exec grep -l $$'\t' {} \; 2>/dev/null | head -10 | grep -q .; then \
		echo "❌ Tab characters found in source files:"; \
		find src/ -name '*.rs' -exec grep -Hn $$'\t' {} \; 2>/dev/null | head -10; \
		echo "Please use spaces for indentation."; \
		exit 1; \
	fi
	@echo "Checking for Windows line endings..."
	@if find src/ -name '*.rs' -exec grep -l $$'\r' {} \; 2>/dev/null | grep -q .; then \
		echo "❌ Windows line endings (CRLF) found in source files:"; \
		find src/ -name '*.rs' -exec grep -l $$'\r' {} \; 2>/dev/null; \
		echo "Run the following to fix: find src/ -name '*.rs' -exec dos2unix {} \\;"; \
		exit 1; \
	fi
	@echo "✅ All linting checks passed!"

# Clean up all artifacts and stop services
clean: stop
	# Stop nginx if running
	@[ -f $(PID_FILE) ] && kill -TERM $$(cat $(PID_FILE)) 2>/dev/null && rm -f $(PID_FILE) || true
	# Clean build artifacts and temp files
	cargo clean
	rm -f /tmp/nginx-ngx-inference-*.log /tmp/nginx-ngx-inference-test.conf
	# Remove nginx temp directories
	rm -rf /tmp/nginx_client_body_temp /tmp/nginx_proxy_temp /tmp/nginx_fastcgi_temp /tmp/nginx_scgi_temp /tmp/nginx_uwsgi_temp
	# Clean echo server node_modules
	rm -rf docker/echo-server/node_modules docker/echo-server/package-lock.json docker/echo-server/package.json

# Service management
start: deploy  # Alias for deploy
start-docker: deploy  # Start full Docker stack

deploy:
	docker compose -f $(DOCKER_COMPOSE_MAIN) up --build -d

start-local: build-check
	@echo "Starting mock external processor..."
	@pkill -f "extproc_mock" || true
	@EPP_UPSTREAM=localhost:8080 MOCK_ROLE=EPP ./target/debug/extproc_mock 0.0.0.0:9001 &
	@echo "Starting echo server..."
	@pkill -f "custom-echo-server.js" || true
	@cd docker/echo-server && [ ! -f package.json ] && npm init -y >/dev/null 2>&1 || true
	@cd docker/echo-server && [ ! -d node_modules ] && npm install express >/dev/null 2>&1 || true
	@cd docker/echo-server && PORT=8080 node custom-echo-server.js &

setup-local:
	./tests/setup-local-dev.sh --local

setup-docker:
	./tests/setup-local-dev.sh --docker

stop:
	docker compose -f $(DOCKER_COMPOSE_MAIN) down --remove-orphans
	# Stop local processes
	pkill -f "custom-echo-server.js" 2>/dev/null || true
	pkill -f "extproc_mock" 2>/dev/null || true

# Testing targets
test:
ifeq ($(TEST_ENV),local)
	$(MAKE) test-local
else
	$(MAKE) test-docker
endif

test-docker:
	@echo "Starting main docker services with build..."
	docker compose -f $(DOCKER_COMPOSE_MAIN) up --build -d
	DOCKER_ENVIRONMENT=main ./tests/test-config.sh

test-local: start-local
	./tests/test-config.sh

# Configuration generation
generate-config:
	@[ -n "$(OUTPUT)" ] || (echo "Error: OUTPUT required. Usage: make generate-config OUTPUT=/path ENV=local|docker TEST=scenario"; exit 1)
	@ENV_ARG=$${ENV:-local}; \
	[ -n "$(TEST)" ] && \
		./tests/generate-config.sh -e $$ENV_ARG -o $(OUTPUT) -s $(TEST) || \
		./tests/generate-config.sh -e $$ENV_ARG -o $(OUTPUT)

# Help target
help:
	@echo "NGX-Inference Makefile"
	@echo ""
	@echo "BUILD TARGETS:"
	@echo "  build          Build both the inference module and mock server"
	@echo "  build-check    Build only if binaries don't exist (fast)"
	@echo "  build-inference Build the ngx-inference module only"
	@echo "  build-mock     Build the mock server only"
	@echo "  check          Quick compilation check without building"
	@echo "  clean          Clean all artifacts and stop services"
	@echo "  lint           Run Rust linting and whitespace checks"
	@echo ""
	@echo "SERVICE MANAGEMENT:"
	@echo "  start/deploy     Start full Docker stack"
	@echo "  start-docker     Start full Docker stack (alias for deploy)"
	@echo "  start-local      Start backend services locally (for local nginx)"
	@echo "  setup-local      Setup and validate local development environment"
	@echo "  setup-docker     Setup and validate Docker development environment"
	@echo "  stop             Stop all services"
	@echo ""
	@echo "TESTING:"
	@echo "  test           Run tests (TEST_ENV=local|docker)"
	@echo "  test-docker    Run Docker-based tests"
	@echo "  test-local     Run local nginx tests (includes BBR large body tests)"
	@echo ""
	@echo "CONFIGURATION:"
	@echo "  generate-config Generate nginx config (OUTPUT=/path ENV=local|docker TEST=scenario)"
	@echo ""
	@echo "EXAMPLES:"
	@echo "  make start                 # Start full stack"
	@echo "  make test TEST_ENV=local   # Run local tests"
	@echo "  make generate-config OUTPUT=/tmp/nginx.conf ENV=local TEST=bbr_on_epp_off"