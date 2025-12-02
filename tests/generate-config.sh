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
KIND_NAMESPACE="ngx-inference-test"

usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Generate nginx configuration from template with appropriate module path.

OPTIONS:
    -e, --environment ENV    Environment: 'local', 'docker', or 'kind' (default: local)
    -o, --output FILE        Output configuration file (required)
    -s, --server CONFIG      Server configuration name (e.g., bbr_on_epp_off)
    -t, --template FILE      Template file (default: nginx-base.conf)
    -n, --namespace NS       Kubernetes namespace for kind (default: ngx-inference-test)
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
        -n|--namespace)
            KIND_NAMESPACE="$2"
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
if [[ "$ENVIRONMENT" != "local" && "$ENVIRONMENT" != "docker" && "$ENVIRONMENT" != "kind" ]]; then
    echo "Error: Environment must be 'local', 'docker', or 'kind'"
    exit 1
fi

# Set module path and endpoints based on environment
if [[ "$ENVIRONMENT" == "local" ]]; then
    if [[ "$(uname)" == "Darwin" ]]; then
        MODULE_PATH="$PROJECT_ROOT/target/debug/libngx_inference.dylib"
        MIMETYPES_PATH="/opt/homebrew/etc/nginx/mime.types"
    else
        MODULE_PATH="$PROJECT_ROOT/target/debug/libngx_inference.so"
        MIMETYPES_PATH="/etc/nginx/mime.types"
    fi
    UPSTREAM_HOST="localhost:8080"
    EPP_HOST="localhost:9001"
    ERROR_LOG="/tmp/nginx-ngx-inference-error.log"
    ACCESS_LOG="/tmp/nginx-ngx-inference-access.log"
elif [[ "$ENVIRONMENT" == "kind" ]]; then
    MODULE_PATH="/usr/lib/nginx/modules/libngx_inference.so"
    MIMETYPES_PATH="/etc/nginx/mime.types"
    # Use echo-server for BBR tests (simple HTTP backend)
    UPSTREAM_HOST="echo-server.${KIND_NAMESPACE}.svc.cluster.local:80"
    # The Helm chart creates the EPP service with -epp suffix
    # Use short service name (not FQDN) for gRPC DNS resolution compatibility
    EPP_HOST="vllm-llama3-8b-instruct-epp:9002"
    # In Kubernetes, log to stdout/stderr for kubectl logs to work
    ERROR_LOG="/dev/stderr"
    ACCESS_LOG="/dev/stdout"
elif [[ "$ENVIRONMENT" == "docker" ]]; then
    MODULE_PATH="/usr/lib/nginx/modules/libngx_inference.so"
    MIMETYPES_PATH="/etc/nginx/mime.types"
    UPSTREAM_HOST="echo-server:80"
    EPP_HOST="mock-epp:9001"
    ERROR_LOG="/tmp/nginx-ngx-inference-error.log"
    ACCESS_LOG="/tmp/nginx-ngx-inference-access.log"
else
    # Default/fallback configuration
    MODULE_PATH="/usr/lib/nginx/modules/libngx_inference.so"
    MIMETYPES_PATH="/etc/nginx/mime.types"
    UPSTREAM_HOST="echo-server:80"
    EPP_HOST="mock-extproc:9001"
    ERROR_LOG="/tmp/nginx-ngx-inference-error.log"
    ACCESS_LOG="/tmp/nginx-ngx-inference-access.log"
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

    # Get the appropriate resolver based on environment
    if [[ "$ENVIRONMENT" == "kind" ]]; then
        # In Kubernetes, use the kube-dns service IP
        RESOLVER="10.96.0.10"
    else
        # For local/docker, use system resolver, filtering out invalid IPv6 addresses
        RESOLVER=$(grep '^nameserver' /etc/resolv.conf | grep -v '^nameserver fe80::' | awk '{print $2}' | head -1 || echo "8.8.8.8")
    fi

    # Read server config and replace localhost references with environment-specific hosts
    SERVER_CONFIG_CONTENT=$(cat "$SERVER_CONFIG_FILE" | \
        sed "s|http://localhost:8080|http://$UPSTREAM_HOST|g" | \
        sed "s|\"localhost:8080\"|\"$UPSTREAM_HOST\"|g" | \
        sed "s|\"127.0.0.1:8080\"|\"$UPSTREAM_HOST\"|g" | \
        sed "s|127.0.0.1:8080|$UPSTREAM_HOST|g" | \
        sed "s|\"localhost:9001\"|\"$EPP_HOST\"|g" | \
        sed "s|\"127.0.0.1:9001\"|\"$EPP_HOST\"|g" | \
        sed "s|\"mock-extproc:9001\"|\"$EPP_HOST\"|g" | \
        sed "s|\"vllm-llama3-8b-instruct-epp:9002\"|\"$EPP_HOST\"|g" | \
        sed "s|localhost:9001|$EPP_HOST|g" | \
        sed "s|127.0.0.1:9001|$EPP_HOST|g" | \
        sed "s|mock-extproc:9001|$EPP_HOST|g")

    # For local and docker environments, disable TLS and remove CA file directive
    # Only kind environment uses TLS with proper certificates
    if [[ "$ENVIRONMENT" == "local" || "$ENVIRONMENT" == "docker" ]]; then
        SERVER_CONFIG_CONTENT=$(echo "$SERVER_CONFIG_CONTENT" | \
            sed "s|inference_epp_tls on;|inference_epp_tls off;|g" | \
            sed "/inference_epp_ca_file/d" | \
            sed 's|inference_epp_header_name "x-gateway-destination-endpoint";|inference_epp_header_name "x-inference-upstream";|g')
    fi

    # Create temporary files
    TMP_SERVER_CONFIG=$(mktemp)
    TMP_OUTPUT=$(mktemp)

    # Write the processed server config to temp file
    echo "$SERVER_CONFIG_CONTENT" > "$TMP_SERVER_CONFIG"

    # Replace module, resolver, and log placeholders first
    sed -e "s|MODULE_PATH_PLACEHOLDER|${MODULE_PATH}|g" \
        -e "s|MIMETYPES_PATH_PLACEHOLDER|${MIMETYPES_PATH}|g" \
        -e "s|RESOLVER_PLACEHOLDER|${RESOLVER}|g" \
        -e "s|ERROR_LOG_PLACEHOLDER|${ERROR_LOG}|g" \
        -e "s|ACCESS_LOG_PLACEHOLDER|${ACCESS_LOG}|g" \
        "$TEMPLATE" > "$TMP_OUTPUT"

    # Now replace the include directive with the actual server config content using awk
    awk -v server_config="$TMP_SERVER_CONFIG" '
        /include TEST_SERVER_CONFIG_PLACEHOLDER;/ {
            while ((getline line < server_config) > 0) {
                print line
            }
            close(server_config)
            next
        }
        { print }
    ' "$TMP_OUTPUT" > "$OUTPUT_FILE"

    # Clean up temp files
    rm -f "$TMP_SERVER_CONFIG" "$TMP_OUTPUT"
else
    # Get the first valid nameserver from /etc/resolv.conf, filtering out invalid IPv6 addresses
    RESOLVER=$(grep '^nameserver' /etc/resolv.conf | grep -v '^nameserver fe80::' | awk '{print $2}' | head -1 || echo "8.8.8.8")

    # Just replace module, resolver, and log placeholders
    sed -e "s|MODULE_PATH_PLACEHOLDER|${MODULE_PATH}|g" \
        -e "s|MIMETYPES_PATH_PLACEHOLDER|${MIMETYPES_PATH}|g" \
        -e "s|RESOLVER_PLACEHOLDER|${RESOLVER}|g" \
        -e "s|ERROR_LOG_PLACEHOLDER|${ERROR_LOG}|g" \
        -e "s|ACCESS_LOG_PLACEHOLDER|${ACCESS_LOG}|g" \
        "$TEMPLATE" > "$OUTPUT_FILE"
fi

echo "✓ Configuration generated successfully: $OUTPUT_FILE"
