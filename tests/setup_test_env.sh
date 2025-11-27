#!/bin/bash

# Setup script for ngx-inference Docker-based tests
# Helps prepare the test environment with required dependencies

cd "$(dirname "$0")/.."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== NGX-INFERENCE DOCKER TEST ENVIRONMENT SETUP ===${NC}"

# Check for required tools
echo -e "${YELLOW}Checking required tools...${NC}"

tools_missing=0

# Check docker compose
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    echo -e "${GREEN}✓ docker compose found: $(docker compose version)${NC}"
else
    echo -e "${RED}✗ docker compose not found - please install Docker Compose v2${NC}"
    tools_missing=1
fi

# Check docker
if command -v docker >/dev/null 2>&1; then
    echo -e "${GREEN}✓ docker found: $(docker --version)${NC}"
else
    echo -e "${RED}✗ docker not found - please install Docker${NC}"
    tools_missing=1
fi

# Check curl
if command -v curl >/dev/null 2>&1; then
    echo -e "${GREEN}✓ curl found: $(curl --version | head -n1)${NC}"
else
    echo -e "${RED}✗ curl not found - please install curl${NC}"
    tools_missing=1
fi

# Check jq
if command -v jq >/dev/null 2>&1; then
    echo -e "${GREEN}✓ jq found: $(jq --version)${NC}"
else
    echo -e "${RED}✗ jq not found - please install jq for JSON parsing${NC}"
    echo -e "  On macOS: brew install jq"
    echo -e "  On Ubuntu: sudo apt-get install jq"
    tools_missing=1
fi

# Check Docker services
echo -e "\n${YELLOW}Checking Docker Compose services...${NC}"

# Check if docker-compose.yml exists
if [[ -f "./docker-compose.yml" ]]; then
    echo -e "${GREEN}✓ docker-compose.yml found${NC}"
else
    echo -e "${RED}✗ docker-compose.yml not found${NC}"
    tools_missing=1
fi

# Try to check service status
if docker compose ps >/dev/null 2>&1; then
    echo -e "${GREEN}✓ Docker Compose is working${NC}"
    
    # Check if services are running
    if docker compose ps --filter "status=running" | grep -q "nginx\|echo-server\|mock-epp"; then
        echo -e "${GREEN}✓ Some services are already running${NC}"
    else
        echo -e "${YELLOW}⚠ Services not running - will start them for testing${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Docker Compose services not accessible (this is normal if not started)${NC}"
fi

# Create testing directory
echo -e "\n${YELLOW}Creating test directories...${NC}"
mkdir -p ./testing
echo -e "${GREEN}✓ Created ./testing directory for temporary files${NC}"

# Check configs directory
if [[ -d "./tests/configs" ]]; then
    echo -e "${GREEN}✓ Test configurations found${NC}"
else
    echo -e "${RED}✗ Test configurations missing${NC}"
    tools_missing=1
fi

# Summary
echo -e "\n${BLUE}=== SETUP SUMMARY ===${NC}"

if [[ $tools_missing -eq 0 ]]; then
    echo -e "${GREEN}✓ All required tools are available${NC}"
    echo -e "\n${GREEN}You can now run the tests:${NC}"
    echo -e "  ./tests/test_docker_simple.sh     - Run configuration toggle tests"
    echo -e "  ./tests/test_large_body.sh        - Run BBR functionality tests"
    echo -e ""
    echo -e "${YELLOW}To start services first (if not running):${NC}"
    echo -e "  docker compose up -d"
else
    echo -e "${RED}✗ Some required tools are missing${NC}"
    echo -e "  Please install the missing tools and run this setup script again."
fi

exit $tools_missing