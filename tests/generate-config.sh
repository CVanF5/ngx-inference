#!/bin/bash

# Utility script to generate nginx configurations from templates
# Supports both local development and Docker/production environments

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Default values
ENVIRONMENT="local"
OUTPUT_FILE=""
SERVER_CONFIG=""
TEMPLATE="$SCRIPT_DIR/configs/nginx-base.conf"

usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Generate nginx configuration from template with appropriate module path.

OPTIONS:
    -e, --environment ENV    Environment: 'local' or 'docker' (default: local)
    -o, --output FILE        Output configuration file (required)
    -s, --server CONFIG      Server configuration name (e.g., bbr_on_epp_off)
    -t, --template FILE      Template file (default: nginx-base.conf)
    -h, --help              Show this help

EXAMPLES:
    # Generate config for local development with BBR on, EPP off
    $0 -e local -o /tmp/nginx.conf -s bbr_on_epp_off

    # Generate config for Docker environment
    $0 -e docker -o /etc/nginx/nginx.conf -s bbr_on_epp_on

    # Generate base config without server-specific settings
    $0 -e local -o /tmp/nginx-base.conf

EOF
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -e|--environment)
            ENVIRONMENT="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT_FILE="$2"
            shift 2
            ;;
        -s|--server)
            SERVER_CONFIG="$2"
            shift 2
            ;;
        -t|--template)
            TEMPLATE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

# Validate required arguments
if [[ -z "$OUTPUT_FILE" ]]; then
    echo "Error: Output file is required (-o/--output)"
    usage
    exit 1
fi

# Validate environment
if [[ "$ENVIRONMENT" != "local" && "$ENVIRONMENT" != "docker" ]]; then
    echo "Error: Environment must be 'local' or 'docker'"
    exit 1
fi

# Set module path based on environment
if [[ "$ENVIRONMENT" == "local" ]]; then
    if [[ "$(uname)" == "Darwin" ]]; then
        MODULE_PATH="$PROJECT_ROOT/target/debug/libngx_inference.dylib"
        MIMETYPES_PATH="/opt/homebrew/etc/nginx/mime.types"
    else
        MODULE_PATH="$PROJECT_ROOT/target/debug/libngx_inference.so"
        MIMETYPES_PATH="/etc/nginx/mime.types"
    fi
else
    MODULE_PATH="/usr/lib/nginx/modules/libngx_inference.so"
    MIMETYPES_PATH="/etc/nginx/mime.types"
fi

# Validate template file exists
if [[ ! -f "$TEMPLATE" ]]; then
    echo "Error: Template file not found: $TEMPLATE"
    exit 1
fi

echo "Generating nginx configuration..."
echo "  Environment: $ENVIRONMENT"
echo "  Template: $TEMPLATE"
echo "  Module path: $MODULE_PATH"
echo "  Mime types path: $MIMETYPES_PATH"
echo "  Output: $OUTPUT_FILE"

# Create output directory if it doesn't exist
mkdir -p "$(dirname "$OUTPUT_FILE")"

# Generate configuration
if [[ -n "$SERVER_CONFIG" ]]; then
    SERVER_CONFIG_FILE="$SCRIPT_DIR/configs/${SERVER_CONFIG}.conf"
    if [[ ! -f "$SERVER_CONFIG_FILE" ]]; then
        echo "Error: Server config file not found: $SERVER_CONFIG_FILE"
        exit 1
    fi
    echo "  Server config: $SERVER_CONFIG_FILE"

    # Get the first nameserver from /etc/resolv.conf
    RESOLVER=$(grep -m1 '^nameserver' /etc/resolv.conf | awk '{print $2}' || echo "8.8.8.8")

    # Replace all placeholders
    sed -e "s|TEST_SERVER_CONFIG_PLACEHOLDER|${SERVER_CONFIG_FILE}|g" \
        -e "s|MODULE_PATH_PLACEHOLDER|${MODULE_PATH}|g" \
        -e "s|MIMETYPES_PATH_PLACEHOLDER|${MIMETYPES_PATH}|g" \
        -e "s|RESOLVER_PLACEHOLDER|${RESOLVER}|g" \
        "$TEMPLATE" > "$OUTPUT_FILE"
else
    # Get the first nameserver from /etc/resolv.conf
    RESOLVER=$(grep -m1 '^nameserver' /etc/resolv.conf | awk '{print $2}' || echo "8.8.8.8")

    # Just replace module and resolver placeholders
    sed -e "s|MODULE_PATH_PLACEHOLDER|${MODULE_PATH}|g" \
        -e "s|MIMETYPES_PATH_PLACEHOLDER|${MIMETYPES_PATH}|g" \
        -e "s|RESOLVER_PLACEHOLDER|${RESOLVER}|g" \
        "$TEMPLATE" > "$OUTPUT_FILE"
fi

echo "✓ Configuration generated successfully: $OUTPUT_FILE"