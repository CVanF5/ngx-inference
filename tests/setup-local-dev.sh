#!/bin/bash

# Setup script for ngx-inference development environment
# Supports both local nginx development and Docker-based testing

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== NGX-INFERENCE DEVELOPMENT ENVIRONMENT SETUP ===${NC}"
echo ""

# Parse command line arguments
ENVIRONMENT=""
SHOW_HELP=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --docker)
            ENVIRONMENT="docker"
            shift
            ;;
        --local)
            ENVIRONMENT="local"
            shift
            ;;
        --help|-h)
            SHOW_HELP=true
            shift
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            SHOW_HELP=true
            shift
            ;;
    esac
done

if [[ "$SHOW_HELP" == "true" ]]; then
    echo "Usage: $0 [--local|--docker] [--help]"
    echo ""
    echo "Options:"
    echo "  --local    Setup for local nginx development (default)"
    echo "  --docker   Setup for Docker-based testing"
    echo "  --help     Show this help message"
    echo ""
    exit 0
fi

# Default to local if no environment specified
if [[ -z "$ENVIRONMENT" ]]; then
    ENVIRONMENT="local"
fi

echo -e "${YELLOW}Setting up for ${ENVIRONMENT} development...${NC}"
echo ""

# Common tools check
echo -e "${YELLOW}Checking common tools...${NC}"
tools_missing=0

# Check curl
if command -v curl >/dev/null 2>&1; then
    echo -e "${GREEN}✓ curl found: $(curl --version | head -n1)${NC}"
else
    echo -e "${RED}✗ curl not found - please install curl${NC}"
    if [[ "$(uname)" == "Darwin" ]]; then
        echo "  brew install curl"
    elif [[ "$(uname)" == "Linux" ]]; then
        echo "  sudo apt-get install curl"
    fi
    tools_missing=1
fi

# Check jq
if command -v jq >/dev/null 2>&1; then
    echo -e "${GREEN}✓ jq found: $(jq --version)${NC}"
else
    echo -e "${RED}✗ jq not found - please install jq for JSON parsing${NC}"
    if [[ "$(uname)" == "Darwin" ]]; then
        echo "  brew install jq"
    elif [[ "$(uname)" == "Linux" ]]; then
        echo "  sudo apt-get install jq"
    fi
    tools_missing=1
fi

echo ""

if [[ "$ENVIRONMENT" == "local" ]]; then
    echo -e "${YELLOW}Checking local development requirements...${NC}"

    # Check nginx
    if command -v nginx >/dev/null 2>&1; then
        CURRENT_VERSION=$(nginx -v 2>&1 | sed 's/nginx version: nginx\///')
        echo -e "${GREEN}✓ nginx found: $CURRENT_VERSION${NC}"
    else
        echo -e "${RED}✗ nginx not found - please install nginx${NC}"
        if [[ "$(uname)" == "Darwin" ]]; then
            echo "  brew install nginx"
        elif [[ "$(uname)" == "Linux" ]]; then
            echo "  # Ubuntu/Debian:"
            echo "  sudo apt-get install nginx"
            echo "  # CentOS/RHEL:"
            echo "  sudo yum install nginx"
        fi
        tools_missing=1
    fi

    # Check Node.js for echo server
    if command -v node >/dev/null 2>&1; then
        echo -e "${GREEN}✓ node found: $(node --version)${NC}"
    else
        echo -e "${RED}✗ node not found - please install Node.js for echo server${NC}"
        if [[ "$(uname)" == "Darwin" ]]; then
            echo "  brew install node"
        elif [[ "$(uname)" == "Linux" ]]; then
            echo "  # Ubuntu/Debian:"
            echo "  sudo apt-get install nodejs npm"
            echo "  # Or use NodeSource:"
            echo "  curl -fsSL https://deb.nodesource.com/setup_lts.x | sudo -E bash -"
            echo "  sudo apt-get install -y nodejs"
        fi
        tools_missing=1
    fi

    # Check npm
    if command -v npm >/dev/null 2>&1; then
        echo -e "${GREEN}✓ npm found: $(npm --version)${NC}"
    else
        echo -e "${RED}✗ npm not found - usually installed with Node.js${NC}"
        tools_missing=1
    fi

    # Check Rust/Cargo for mock external processor
    if command -v cargo >/dev/null 2>&1; then
        echo -e "${GREEN}✓ cargo found: $(cargo --version)${NC}"
    else
        echo -e "${RED}✗ cargo not found - please install Rust${NC}"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        echo "  source ~/.cargo/env"
        tools_missing=1
    fi

elif [[ "$ENVIRONMENT" == "docker" ]]; then
    echo -e "${YELLOW}Checking Docker development requirements...${NC}"

    # Check docker
    if command -v docker >/dev/null 2>&1; then
        echo -e "${GREEN}✓ docker found: $(docker --version)${NC}"
    else
        echo -e "${RED}✗ docker not found - please install Docker${NC}"
        tools_missing=1
    fi

    # Check docker compose
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
        echo -e "${GREEN}✓ docker compose found: $(docker compose version)${NC}"
    else
        echo -e "${RED}✗ docker compose not found - please install Docker Compose v2${NC}"
        tools_missing=1
    fi

    # Check if docker-compose.yml exists
    if [[ -f "$PROJECT_ROOT/tests/docker-compose.yml" ]]; then
        echo -e "${GREEN}✓ docker-compose.yml found${NC}"
    else
        echo -e "${RED}✗ docker-compose.yml not found in tests/${NC}"
        tools_missing=1
    fi
fi

echo ""

# Create necessary directories
echo -e "${YELLOW}Creating necessary directories...${NC}"

# Create nginx temp directories
mkdir -p /tmp/nginx_client_body_temp /tmp/nginx_proxy_temp /tmp/nginx_fastcgi_temp /tmp/nginx_scgi_temp /tmp/nginx_uwsgi_temp
echo -e "${GREEN}✓ Created nginx temp directories in /tmp/${NC}"

# Check test configurations
if [[ -d "$PROJECT_ROOT/tests/configs" ]]; then
    echo -e "${GREEN}✓ Test configurations found${NC}"
else
    echo -e "${RED}✗ Test configurations missing in tests/configs/${NC}"
    tools_missing=1
fi

echo ""

# Summary and next steps
echo -e "${BLUE}=== SETUP SUMMARY ===${NC}"

if [[ $tools_missing -eq 0 ]]; then
    echo -e "${GREEN}✓ All required tools are available for ${ENVIRONMENT} development${NC}"
    echo ""

    if [[ "$ENVIRONMENT" == "local" ]]; then
        echo -e "${GREEN}Ready for local development!${NC}"
        echo ""
        echo -e "${YELLOW}To run local tests:${NC}"
        echo "  make test-local              # Run tests with local nginx and services"
        echo "  make start-local             # Start backend services only"
        echo "  make clean                   # Clean up all services and artifacts"
        echo ""
        echo -e "${YELLOW}Module build info:${NC}"
        if command -v nginx >/dev/null 2>&1; then
            NGINX_VERSION=$(nginx -v 2>&1 | sed 's/nginx version: nginx\///')
            echo "  Module will be built for nginx $NGINX_VERSION using vendored nginx sources"
            echo "  GPG verification is disabled for local development builds"
        fi

    elif [[ "$ENVIRONMENT" == "docker" ]]; then
        echo -e "${GREEN}Ready for Docker-based testing!${NC}"
        echo ""
        echo -e "${YELLOW}To run Docker tests:${NC}"
        echo "  make test-docker             # Run full Docker-based tests"
        echo "  make deploy                  # Start full application stack"
        echo "  make stop                    # Stop all Docker services"
        echo ""
        echo -e "${YELLOW}To start services manually:${NC}"
        echo "  docker compose -f tests/docker-compose.yml up -d"
    fi

else
    echo -e "${RED}✗ Some required tools are missing for ${ENVIRONMENT} development${NC}"
    echo "  Please install the missing tools and run this script again."
    echo ""
    echo "  You can also try the other environment:"
    if [[ "$ENVIRONMENT" == "local" ]]; then
        echo "    $0 --docker    # Setup for Docker-based testing"
    else
        echo "    $0 --local     # Setup for local development"
    fi
fi

exit $tools_missing