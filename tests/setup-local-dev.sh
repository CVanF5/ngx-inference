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
        --kind)
            ENVIRONMENT="kind"
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
    echo "Usage: $0 [--local|--docker|--kind] [--help]"
    echo ""
    echo "Options:"
    echo "  --local    Setup for local nginx development (default)"
    echo "  --docker   Setup for Docker-based testing"
    echo "  --kind     Setup for KIND-based testing"
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

# Detect OS and package manager
detect_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS=$ID
        OS_VERSION=$VERSION_ID
    elif [[ "$(uname)" == "Darwin" ]]; then
        OS="macos"
    else
        OS="unknown"
    fi
}

# Check if OS is part of RedHat family
is_rhel_family() {
    [[ "$OS" == "centos" || "$OS" == "rhel" || "$OS" == "fedora" || "$OS" == "rocky" || "$OS" == "almalinux" ]]
}

detect_os

# Common tools check
echo -e "${YELLOW}Checking common tools...${NC}"
tools_missing=0

# Check curl
if command -v curl >/dev/null 2>&1; then
    echo -e "${GREEN} curl found: $(curl --version | head -n1)${NC}"
else
    echo -e "${RED} curl not found - please install curl${NC}"
    if [[ "$OS" == "macos" ]]; then
        echo "  brew install curl"
    elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
        echo "  sudo apt-get install curl"
    fi
    tools_missing=1
fi

# Check jq
if command -v jq >/dev/null 2>&1; then
    echo -e "${GREEN} jq found: $(jq --version)${NC}"
else
    echo -e "${RED} jq not found - please install jq for JSON parsing${NC}"
    if [[ "$OS" == "macos" ]]; then
        echo "  brew install jq"
    elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
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
        echo -e "${GREEN} nginx found: $CURRENT_VERSION${NC}"
    else
        echo -e "${RED} nginx not found - please install nginx${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  brew install nginx"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install nginx"
        elif is_rhel_family; then
            echo "  sudo dnf install nginx"
        fi
        tools_missing=1
    fi

    # Check Node.js for echo server
    if command -v node >/dev/null 2>&1; then
        echo -e "${GREEN} node found: $(node --version)${NC}"
    else
        echo -e "${RED} node not found - please install Node.js for echo server${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  brew install node"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install nodejs npm"
        fi
        tools_missing=1
    fi

    # Check npm
    if command -v npm >/dev/null 2>&1; then
        echo -e "${GREEN} npm found: $(npm --version)${NC}"
    else
        echo -e "${RED} npm not found - usually installed with Node.js${NC}"
        tools_missing=1
    fi

    # Check Rust/Cargo for mock external processor
    if command -v cargo >/dev/null 2>&1; then
        echo -e "${GREEN} cargo found: $(cargo --version)${NC}"
    else
        echo -e "${RED} cargo not found - please install Rust${NC}"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        echo "  source ~/.cargo/env"
        tools_missing=1
    fi

    echo ""
    echo -e "${YELLOW}Checking Rust build dependencies...${NC}"

    # Check clang/LLVM (required for bindgen)
    if command -v clang >/dev/null 2>&1; then
        echo -e "${GREEN} clang found: $(clang --version | head -n1)${NC}"
    else
        echo -e "${RED} clang not found - required for Rust bindgen${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  xcode-select --install"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install clang"
        elif [[ "$OS" == "alpine" ]]; then
            echo "  apk add clang-dev"
        elif is_rhel_family; then
            echo "  sudo dnf install clang"
        fi
        tools_missing=1
    fi

    # Check PCRE2 development files
    pcre2_found=false
    if [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
        if dpkg -l | grep -q libpcre2-dev; then
            echo -e "${GREEN} libpcre2-dev found${NC}"
            pcre2_found=true
        fi
    elif [[ "$OS" == "macos" ]]; then
        if brew list pcre2 &>/dev/null; then
            echo -e "${GREEN} pcre2 found${NC}"
            pcre2_found=true
        fi
    elif is_rhel_family; then
        if rpm -q pcre2-devel &>/dev/null; then
            echo -e "${GREEN} pcre2-devel found${NC}"
            pcre2_found=true
        fi
    fi

    if [[ "$pcre2_found" == "false" ]]; then
        echo -e "${RED} PCRE2 development files not found - required for nginx module build${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  brew install pcre2"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install libpcre2-dev"
        elif [[ "$OS" == "alpine" ]]; then
            echo "  apk add pcre2-dev"
        elif is_rhel_family; then
            echo "  sudo dnf install pcre2-devel"
        fi
        tools_missing=1
    fi

    # Check OpenSSL development files
    openssl_found=false
    if [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
        if dpkg -l | grep -q libssl-dev; then
            echo -e "${GREEN} libssl-dev found${NC}"
            openssl_found=true
        fi
    elif [[ "$OS" == "macos" ]]; then
        if brew list openssl &>/dev/null; then
            echo -e "${GREEN} openssl found${NC}"
            openssl_found=true
        fi
    elif is_rhel_family; then
        if rpm -q openssl-devel &>/dev/null; then
            echo -e "${GREEN} openssl-devel found${NC}"
            openssl_found=true
        fi
    fi

    if [[ "$openssl_found" == "false" ]]; then
        echo -e "${RED} OpenSSL development files not found - required for nginx module build${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  brew install openssl"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install libssl-dev"
        elif [[ "$OS" == "alpine" ]]; then
            echo "  apk add openssl-dev"
        elif is_rhel_family; then
            echo "  sudo dnf install openssl-devel"
        fi
        tools_missing=1
    fi

    # Check zlib development files
    zlib_found=false
    if [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
        if dpkg -l | grep -q zlib1g-dev; then
            echo -e "${GREEN} zlib1g-dev found${NC}"
            zlib_found=true
        fi
    elif [[ "$OS" == "macos" ]]; then
        if brew list zlib &>/dev/null; then
            echo -e "${GREEN} zlib found${NC}"
            zlib_found=true
        fi
    elif is_rhel_family; then
        if rpm -q zlib-devel &>/dev/null; then
            echo -e "${GREEN} zlib-devel found${NC}"
            zlib_found=true
        fi
    fi

    if [[ "$zlib_found" == "false" ]]; then
        echo -e "${RED} zlib development files not found - required for nginx gzip module${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  brew install zlib"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install zlib1g-dev"
        elif [[ "$OS" == "alpine" ]]; then
            echo "  apk add zlib-dev"
        elif is_rhel_family; then
            echo "  sudo dnf install zlib-devel"
        fi
        tools_missing=1
    fi

    # Check make
    if command -v make >/dev/null 2>&1; then
        echo -e "${GREEN} make found: $(make --version | head -n1)${NC}"
    else
        echo -e "${RED} make not found - required for nginx module build${NC}"
        if [[ "$OS" == "macos" ]]; then
            echo "  xcode-select --install"
        elif [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo "  sudo apt-get install build-essential"
        elif [[ "$OS" == "alpine" ]]; then
            echo "  apk add make"
        elif is_rhel_family; then
            echo "  sudo dnf install make"
        fi
        tools_missing=1
    fi

elif [[ "$ENVIRONMENT" == "docker" ]]; then
    echo -e "${YELLOW}Checking Docker development requirements...${NC}"

    # Check docker
    if command -v docker >/dev/null 2>&1; then
        echo -e "${GREEN} docker found: $(docker --version)${NC}"
    else
        echo -e "${RED} docker not found - please install Docker${NC}"
        tools_missing=1
    fi

    # Check docker compose
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
        echo -e "${GREEN} docker compose found: $(docker compose version)${NC}"
    else
        echo -e "${RED} docker compose not found - please install Docker Compose v2${NC}"
        tools_missing=1
    fi

    # Check if docker-compose.yml exists
    if [[ -f "$PROJECT_ROOT/tests/docker-compose.yml" ]]; then
        echo -e "${GREEN} docker-compose.yml found${NC}"
    else
        echo -e "${RED} docker-compose.yml not found in tests/${NC}"
        tools_missing=1
    fi

elif [[ "$ENVIRONMENT" == "kind" ]]; then
    echo -e "${YELLOW}Checking KIND development requirements...${NC}"

    # Check kind
    if command -v kind >/dev/null 2>&1; then
        echo -e "${GREEN} kind found${NC}"
    else
        echo -e "${RED} kind not found. Install from: https://kind.sigs.k8s.io/docs/user/quick-start/#installation${NC}"
        tools_missing=1
    fi

    # Check kubectl
    if command -v kubectl >/dev/null 2>&1; then
        echo -e "${GREEN} kubectl found${NC}"
    else
        echo -e "${RED} kubectl not found. Install from: https://kubernetes.io/docs/tasks/tools/${NC}"
        tools_missing=1
    fi

    # Check helm
    if command -v helm >/dev/null 2>&1; then
        echo -e "${GREEN} helm found${NC}"
    else
        echo -e "${RED} helm not found. Install from: https://helm.sh/docs/intro/install/${NC}"
        tools_missing=1
    fi

    # Check docker (needed for kind)
    if command -v docker >/dev/null 2>&1; then
        echo -e "${GREEN} docker found${NC}"
    else
        echo -e "${RED} docker not found. Install from: https://docs.docker.com/get-docker/${NC}"
        tools_missing=1
    fi

fi

echo ""

# Create necessary directories
echo -e "${YELLOW}Creating necessary directories...${NC}"

# Create nginx temp directories
mkdir -p /tmp/nginx_client_body_temp /tmp/nginx_proxy_temp /tmp/nginx_fastcgi_temp /tmp/nginx_scgi_temp /tmp/nginx_uwsgi_temp
echo -e "${GREEN} Created nginx temp directories in /tmp/${NC}"

# Check test configurations
if [[ -d "$PROJECT_ROOT/tests/configs" ]]; then
    echo -e "${GREEN} Test configurations found${NC}"
else
    echo -e "${RED} Test configurations missing in tests/configs/${NC}"
    tools_missing=1
fi

echo ""

# Summary and next steps
echo -e "${BLUE}=== SETUP SUMMARY ===${NC}"

if [[ $tools_missing -eq 0 ]]; then
    echo -e "${GREEN} All required tools are available for ${ENVIRONMENT} development${NC}"
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

    elif [[ "$ENVIRONMENT" == "kind" ]]; then
        echo -e "${GREEN}Ready for KIND-based testing!${NC}"
        echo ""
        echo -e "${YELLOW}To run KIND tests:${NC}"
        echo "  make test-kind               # Run tests against reference EPP in kind cluster"
        echo "  make start-kind              # Create kind cluster and deploy components"
        echo "  make stop                    # Stop all services including kind cluster"
        echo ""
        echo -e "${YELLOW}To manage cluster manually:${NC}"
        echo "  ./tests/kind-ngf/scripts/setup.sh    # Setup cluster and deploy"
        echo "  ./tests/kind-ngf/scripts/test-kind.sh # Run tests"
    fi

else
    echo -e "${RED} Some required tools are missing for ${ENVIRONMENT} development${NC}"
    echo "  Please install the missing tools and run this script again."
    echo ""

    # Provide quick install commands based on OS
    if [[ "$ENVIRONMENT" == "local" ]]; then
        if [[ "$OS" == "debian" || "$OS" == "ubuntu" ]]; then
            echo -e "${YELLOW}Quick install for Debian/Ubuntu:${NC}"
            echo "  sudo apt-get update"
            echo "  sudo apt-get install -y clang libpcre2-dev libssl-dev zlib1g-dev build-essential nginx nodejs npm curl jq"
            echo ""
        elif is_rhel_family; then
            echo -e "${YELLOW}Quick install for RedHat/CentOS/Fedora/Rocky/AlmaLinux:${NC}"
            echo "  sudo dnf install -y clang pcre2-devel openssl-devel zlib-devel make gcc-c++ nginx nodejs npm curl jq"
            echo ""
            if [[ "$OS" == "rhel" || "$OS" == "centos" || "$OS" == "rocky" || "$OS" == "almalinux" ]]; then
                echo -e "${YELLOW}Note: On RHEL/CentOS/Rocky/AlmaLinux, you may need to enable EPEL repository first:${NC}"
                echo "  sudo dnf install -y epel-release"
                echo ""
            fi
        fi
    fi

    echo "  You can also try other environments:"
    if [[ "$ENVIRONMENT" == "local" ]]; then
        echo "    $0 --docker    # Setup for Docker-based testing"
        echo "    $0 --kind      # Setup for KIND-based testing"
    elif [[ "$ENVIRONMENT" == "docker" ]]; then
        echo "    $0 --local     # Setup for local development"
        echo "    $0 --kind      # Setup for KIND-based testing"
    elif [[ "$ENVIRONMENT" == "kind" ]]; then
        echo "    $0 --local     # Setup for local development"
        echo "    $0 --docker    # Setup for Docker-based testing"
    fi
fi

exit $tools_missing
# ASCII-cleaned to eliminate all Unicode and tab issues - Mon  1 Dec 2025 20:45:08 GMT
