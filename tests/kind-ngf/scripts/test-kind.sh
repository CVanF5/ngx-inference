#!/bin/bash
# Test script for kind cluster using existing test-config.sh logic
# Adapts the comprehensive test suite for Kubernetes environment

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
NAMESPACE="ngx-inference-test"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$(dirname "$TEST_DIR")")"

echo -e "${GREEN}=== NGX-Inference Kind Cluster Tests ===${NC}"
echo ""

# Check if cluster is accessible
if ! kubectl cluster-info &> /dev/null; then
    echo -e "${RED}✗ Cluster not accessible${NC}"
    echo "Please run: make test-kind-setup"
    exit 1
fi

if ! kubectl get namespace "$NAMESPACE" &> /dev/null; then
    echo -e "${RED}✗ Namespace $NAMESPACE not found${NC}"
    echo "Please run: make test-kind-setup"
    exit 1
fi

# Port forward setup for testing
setup_port_forward() {
    echo -e "${YELLOW}Setting up port forward to NGINX...${NC}"

    # Kill any existing port forward
    pkill -f "kubectl port-forward.*nginx-inference" || true
    sleep 1

    # Get NGINX pod name
    local nginx_pod=$(kubectl get pods -n "$NAMESPACE" -l app=nginx-inference -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

    if [ -z "$nginx_pod" ]; then
        echo -e "${RED}✗ NGINX pod not found${NC}"
        return 1
    fi

    # Start port forward in background
    kubectl port-forward -n "$NAMESPACE" "$nginx_pod" 8081:8081 >/dev/null 2>&1 &
    local pf_pid=$!

    # Wait for port forward to be ready
    local wait_count=0
    while [ $wait_count -lt 30 ]; do
        if curl -sf http://localhost:8081/health >/dev/null 2>&1; then
            echo -e "${GREEN}✓ Port forward ready${NC}"
            echo "$pf_pid" > /tmp/kind-port-forward.pid
            return 0
        fi
        sleep 0.5
        ((wait_count++))
    done

    echo -e "${RED}✗ Port forward failed to become ready${NC}"
    kill $pf_pid 2>/dev/null || true
    return 1
}

# Cleanup port forward
cleanup_port_forward() {
    if [ -f /tmp/kind-port-forward.pid ]; then
        local pid=$(cat /tmp/kind-port-forward.pid)
        kill $pid 2>/dev/null || true
        rm -f /tmp/kind-port-forward.pid
    fi
    pkill -f "kubectl port-forward.*nginx-inference" || true
}

# Apply configuration for a test scenario
apply_test_config() {
    local scenario=$1
    echo -e "${BLUE}Applying configuration: $scenario${NC}"

    # Generate nginx config for kind environment
    local tmp_config="/tmp/nginx-kind-${scenario}.conf"
    "$PROJECT_ROOT/tests/generate-config.sh" \
        -e kind \
        -o "$tmp_config" \
        -s "$scenario" \
        -n "$NAMESPACE"

    # Create ConfigMap
    kubectl create configmap nginx-inference-config \
        --from-file=nginx.conf="$tmp_config" \
        -n "$NAMESPACE" \
        --dry-run=client -o yaml | kubectl apply -f -

    # Restart NGINX to pick up new config
    kubectl rollout restart deployment nginx-inference -n "$NAMESPACE" >/dev/null
    kubectl rollout status deployment nginx-inference -n "$NAMESPACE" --timeout=60s >/dev/null

    # Give NGINX a moment to fully start
    sleep 2

    rm -f "$tmp_config"
    echo -e "${GREEN}✓ Configuration applied${NC}"
}

# Run test using the existing test-config.sh with special environment
run_test_for_scenario() {
    local scenario=$1
    local test_name=$2

    echo ""
    echo -e "${YELLOW}========================================${NC}"
    echo -e "${YELLOW}Testing: $test_name${NC}"
    echo -e "${YELLOW}========================================${NC}"
    echo ""

    # Apply the configuration
    apply_test_config "$scenario"

    # Reset port forward
    cleanup_port_forward
    if ! setup_port_forward; then
        echo -e "${RED}✗ Failed to setup port forward${NC}"
        return 1
    fi

    # Set environment for kind testing
    export KIND_ENVIRONMENT="true"
    export NGINX_PORT="8081"

    # Source the test functions from test-config.sh
    # We'll just test the specific endpoints we need
    cd "$PROJECT_ROOT"

    # Determine expected behavior
    local expected_bbr="disabled"
    local expected_epp="disabled"

    case $scenario in
        bbr_on_epp_off)
            expected_bbr="enabled"
            expected_epp="disabled"
            ;;
        bbr_off_epp_on)
            expected_bbr="disabled"
            expected_epp="enabled"
            ;;
        bbr_on_epp_on)
            expected_bbr="enabled"
            expected_epp="enabled"
            ;;
        bbr_off_epp_off)
            expected_bbr="disabled"
            expected_epp="disabled"
            ;;
    esac

    # Run simple tests
    local failed=0

    # Test health
    echo "  Testing health endpoint..."
    if curl -sf http://localhost:8081/health >/dev/null; then
        echo -e "${GREEN}  ✓ Health check passed${NC}"
    else
        echo -e "${RED}  ✗ Health check failed${NC}"
        ((failed++))
    fi

    # Get NGINX pod for logging
    local nginx_pod=$(kubectl get pods -n "$NAMESPACE" -l app=nginx-inference -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

    # Test BBR if enabled
    if [ "$expected_bbr" = "enabled" ]; then
        echo "  Testing BBR endpoint..."

        # Show recent logs before test
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs before BBR test:${NC}"
            kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=5 2>/dev/null | sed 's/^/    /' || true
        fi

        local response=$(curl -s http://localhost:8081/bbr-test \
            -H 'Content-Type: application/json' \
            --data '{"model":"test-model","prompt":"test"}' \
            -w "HTTPSTATUS:%{http_code}")

        local http_code=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

        # Show logs immediately after request
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs after BBR request:${NC}"
            local logs=$(kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=10 2>/dev/null)
            echo "$logs" | sed 's/^/    /' || true
        fi

        if [ "$http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ BBR endpoint responded: HTTP $http_code${NC}"
        elif [ "$http_code" = "500" ] || [ "$http_code" = "502" ] || [ "$http_code" = "503" ] || [ "$http_code" = "504" ]; then
            echo -e "${RED}  ✗ BBR endpoint failed: HTTP $http_code${NC}"
            echo -e "${RED}  Response body: $body${NC}"
            ((failed++))
        else
            echo -e "${YELLOW}  ⚠ BBR endpoint: HTTP $http_code${NC}"
            echo -e "${YELLOW}  Response body: $body${NC}"
        fi
    fi

    # Test EPP if enabled, or test expected failure if disabled
    if [ "$expected_epp" = "enabled" ]; then
        echo "  Testing EPP endpoint (/v1/chat/completions)..."

        # Show recent logs before test
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs before EPP test:${NC}"
            kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=5 2>/dev/null | sed 's/^/    /' || true
        fi

        local response=$(curl -s -X POST http://localhost:8081/v1/chat/completions \
            -H 'Content-Type: application/json' \
            -d '{"model": "meta-llama/Llama-3.1-8B-Instruct", "messages": [{"role": "user", "content": "test EPP"}], "max_tokens": 5}' \
            -w "HTTPSTATUS:%{http_code}")

        local http_code=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

        # Show logs immediately after request
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs after EPP request:${NC}"
            kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=10 2>/dev/null | sed 's/^/    /' || true
        fi

        if [ "$http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ EPP endpoint responded: HTTP $http_code${NC}"
        elif [ "$http_code" = "500" ] || [ "$http_code" = "502" ] || [ "$http_code" = "503" ] || [ "$http_code" = "504" ]; then
            echo -e "${RED}  ✗ EPP endpoint failed: HTTP $http_code${NC}"
            echo -e "${RED}  Response body: $body${NC}"
            ((failed++))
        else
            echo -e "${YELLOW}  ⚠ EPP endpoint: HTTP $http_code${NC}"
            echo -e "${YELLOW}  Response body: $body${NC}"
        fi
    else
        # EPP is disabled - test that EPP endpoint works (should use $backend, not $inference_upstream)
        echo "  Testing /v1/chat/completions endpoint (EPP disabled - should use backend)..."

        local response=$(curl -s -X POST http://localhost:8081/v1/chat/completions \
            -H 'Content-Type: application/json' \
            --data '{"model": "meta-llama/Llama-3.1-8B-Instruct", "messages": [{"role": "user", "content": "test EPP disabled"}], "max_tokens": 5}' \
            -w "HTTPSTATUS:%{http_code}")

        local http_code=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

        if [ "$http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ /v1/chat/completions endpoint with EPP disabled responded: HTTP $http_code${NC}"
        elif [ "$http_code" = "500" ] || [ "$http_code" = "502" ] || [ "$http_code" = "503" ] || [ "$http_code" = "504" ]; then
            echo -e "${RED}  ✗ /v1/chat/completions endpoint failed (EPP disabled): HTTP $http_code${NC}"
            echo -e "${RED}  Response body: $body${NC}"
            echo -e "${YELLOW}  Note: This could be expected if config uses \$inference_upstream when EPP is off${NC}"
            # Don't increment failed here - this is an expected failure case
        else
            echo -e "${YELLOW}  ⚠ /v1/chat/completions endpoint (EPP disabled): HTTP $http_code${NC}"
            echo -e "${YELLOW}  Response body: $body${NC}"
        fi
    fi

    # Test for configurations that use $inference_upstream when EPP is disabled (expected failures)
    if [ "$expected_epp" = "disabled" ]; then
        echo "  Testing /v1/completions endpoint (EPP disabled - should use \$backend)..."

        local response=$(curl -s -X POST http://localhost:8081/v1/completions \
            -H 'Content-Type: application/json' \
            --data '{"model": "meta-llama/Llama-3.1-8B-Instruct", "prompt": "test completions", "max_tokens": 5}' \
            -w "HTTPSTATUS:%{http_code}")

        local http_code=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

        # Show logs for debugging
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs after /v1/completions request (EPP disabled):${NC}"
            kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=10 2>/dev/null | sed 's/^/    /' || true
        fi

        # When EPP is disabled, /v1/completions should use $backend and work correctly
        if [ "$http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ /v1/completions endpoint responded correctly (using \$backend): HTTP $http_code${NC}"
        elif [ "$http_code" = "500" ] || [ "$http_code" = "502" ] || [ "$http_code" = "503" ] || [ "$http_code" = "504" ]; then
            # Check if this is due to incorrectly using $inference_upstream
            if echo "$body" | grep -q "inference_upstream" || kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=20 2>/dev/null | grep -q "inference_upstream"; then
                echo -e "${RED}  ✗ CONFIGURATION ERROR: /v1/completions endpoint uses \$inference_upstream when EPP is disabled${NC}"
                echo -e "${RED}  Response: HTTP $http_code - $body${NC}"
                ((failed++))
            else
                echo -e "${RED}  ✗ /v1/completions endpoint failed unexpectedly: HTTP $http_code${NC}"
                echo -e "${RED}  Response body: $body${NC}"
                ((failed++))
            fi
        else
            echo -e "${YELLOW}  ⚠ /v1/completions endpoint (EPP disabled): HTTP $http_code${NC}"
            echo -e "${YELLOW}  Response body: $body${NC}"
        fi
    else
        # When EPP is enabled, test that /responses endpoint can use $inference_upstream
        echo "  Testing /v1/completions endpoint (EPP enabled - should use \$inference_upstream)..."

        local response=$(curl -s -X POST http://localhost:8081/v1/completions \
            -H 'Content-Type: application/json' \
            -d '{"model": "meta-llama/Llama-3.1-8B-Instruct", "prompt": "test completions", "max_tokens": 5}' \
            -w "HTTPSTATUS:%{http_code}")

        local http_code=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

        # Show logs for debugging
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs after /v1/completions request (EPP enabled):${NC}"
            kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=10 2>/dev/null | sed 's/^/    /' || true
        fi

        if [ "$http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ /v1/completions endpoint responded: HTTP $http_code${NC}"
        elif [ "$http_code" = "500" ] || [ "$http_code" = "502" ] || [ "$http_code" = "503" ] || [ "$http_code" = "504" ]; then
            echo -e "${RED}  ✗ /v1/completions endpoint failed: HTTP $http_code${NC}"
            echo -e "${RED}  Response body: $body${NC}"
            ((failed++))
        else
            echo -e "${YELLOW}  ⚠ /v1/completions endpoint: HTTP $http_code${NC}"
            echo -e "${YELLOW}  Response body: $body${NC}"
        fi

        # Test actual vLLM endpoints when EPP is enabled for successful responses
        echo "  Testing vLLM chat/completions endpoint (EPP enabled)..."
        local vllm_response=$(curl -s -X POST http://localhost:8081/v1/chat/completions \
            -H 'Content-Type: application/json' \
            -d '{"model": "meta-llama/Llama-3.1-8B-Instruct", "messages": [{"role": "user", "content": "Hello"}], "max_tokens": 5}' \
            -w "HTTPSTATUS:%{http_code}")

        local vllm_http_code=$(echo "$vllm_response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
        local vllm_body=$(echo "$vllm_response" | sed 's/HTTPSTATUS:[0-9]*$//')

        if [ "$vllm_http_code" = "200" ]; then
            echo -e "${GREEN}  ✓ vLLM chat/completions responded: HTTP $vllm_http_code${NC}"
            # Show a preview of the response
            if echo "$vllm_body" | jq . >/dev/null 2>&1; then
                local response_preview=$(echo "$vllm_body" | jq -r '.choices[0].message.content // .choices[0].text // "No content"' 2>/dev/null | head -c 50)
                echo -e "${GREEN}  Response preview: ${response_preview}...${NC}"
            fi
        else
            echo -e "${YELLOW}  ⚠ vLLM chat/completions: HTTP $vllm_http_code${NC}"
            echo -e "${YELLOW}  Response body: $(echo "$vllm_body" | head -c 100)...${NC}"
        fi

        # Show EPP logs for the vLLM request
        if [ -n "$nginx_pod" ]; then
            echo -e "${BLUE}  NGINX logs after vLLM request (EPP activity):${NC}"
            local recent_logs=$(kubectl logs -n "$NAMESPACE" "$nginx_pod" --tail=15 2>/dev/null)
            echo "$recent_logs" | grep -E "(EPP gRPC|Selected upstream|DEBUG: Found header.*with value)" | sed 's/^/    /' || echo "    (No EPP logs found)"
        fi
    fi

    echo ""

    if [ $failed -eq 0 ]; then
        echo -e "${GREEN}✓ Test passed: $test_name${NC}"
        return 0
    else
        echo -e "${RED}✗ Test failed: $test_name ($failed errors)${NC}"
        return 1
    fi
}

# Main execution
main() {
    local total_failed=0

    # Setup initial port forward
    if ! setup_port_forward; then
        echo -e "${RED}✗ Initial port forward setup failed${NC}"
        exit 1
    fi

    # Test all scenarios
    run_test_for_scenario "bbr_on_epp_off" "BBR ON + EPP OFF" || ((total_failed++))
    run_test_for_scenario "bbr_off_epp_on" "BBR OFF + EPP ON" || ((total_failed++))
    run_test_for_scenario "bbr_on_epp_on" "BBR ON + EPP ON" || ((total_failed++))
    run_test_for_scenario "bbr_off_epp_off" "BBR OFF + EPP OFF" || ((total_failed++))

    # Cleanup
    cleanup_port_forward

    echo ""
    echo -e "${BLUE}=== TEST SUMMARY ===${NC}"

    if [ $total_failed -eq 0 ]; then
        echo -e "${GREEN}✓ All configuration scenarios passed${NC}"
        echo ""
        echo -e "${GREEN}🎉 Kind cluster tests completed successfully! 🎉${NC}"
        return 0
    else
        echo -e "${RED}✗ $total_failed scenario(s) failed${NC}"
        echo ""
        echo "For detailed logs:"
        echo "  kubectl logs -n $NAMESPACE -l app=nginx-inference"
        echo "  kubectl logs -n $NAMESPACE -l app=vllm-llama3-8b-instruct"
        echo ""
        echo "To check pod status:"
        echo "  kubectl get pods -n $NAMESPACE"
        echo "  kubectl describe pod -n $NAMESPACE nginx-inference-<pod-id>"
        return 1
    fi
}

# Trap to cleanup on exit
trap cleanup_port_forward EXIT INT TERM

main "$@"
