#!/bin/bash
# Automated setup script for kind cluster with reference EPP testing
# This script creates a kind cluster, deploys vLLM, installs the reference EPP,
# and deploys NGINX with the ngx-inference module

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
CLUSTER_NAME="ngx-inference-test"
NAMESPACE="ngx-inference-test"
IGW_CHART_VERSION="${IGW_CHART_VERSION:-v1.1.0}"

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$(dirname "$TEST_DIR")")"

echo -e "${GREEN}=== NGX-Inference Reference EPP Test Setup ===${NC}"
echo "Project root: $PROJECT_ROOT"
echo "Test directory: $TEST_DIR"
echo ""

# Check prerequisites
check_prerequisites() {
    echo -e "${YELLOW}Checking prerequisites...${NC}"
    
    local missing=0
    
    if ! command -v kind &> /dev/null; then
        echo -e "${RED}✗ kind not found${NC}"
        missing=1
    else
        echo -e "${GREEN}✓ kind found${NC}"
    fi
    
    if ! command -v kubectl &> /dev/null; then
        echo -e "${RED}✗ kubectl not found${NC}"
        missing=1
    else
        echo -e "${GREEN}✓ kubectl found${NC}"
    fi
    
    if ! command -v docker &> /dev/null; then
        echo -e "${RED}✗ docker not found${NC}"
        missing=1
    else
        echo -e "${GREEN}✓ docker found${NC}"
    fi
    
    if ! command -v helm &> /dev/null; then
        echo -e "${RED}✗ helm not found${NC}"
        missing=1
    else
        echo -e "${GREEN}✓ helm found${NC}"
    fi
    
    if [ $missing -eq 1 ]; then
        echo -e "${RED}Missing required tools. Please install them and try again.${NC}"
        exit 1
    fi
    
    echo ""
}

# Create kind cluster
create_cluster() {
    echo -e "${YELLOW}Creating kind cluster: $CLUSTER_NAME${NC}"
    
    if kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
        echo -e "${YELLOW}Cluster $CLUSTER_NAME already exists. Deleting...${NC}"
        kind delete cluster --name "$CLUSTER_NAME"
    fi
    
    kind create cluster --config "$TEST_DIR/cluster/kind-config.yaml"
    
    echo -e "${GREEN}✓ Cluster created${NC}"
    echo ""
}

# Build and load images
build_and_load_image() {
    echo -e "${YELLOW}Building NGINX image with ngx-inference module...${NC}"
    
    cd "$PROJECT_ROOT"
    docker build -f docker/nginx/Dockerfile -t ngx-inference:latest .
    
    echo -e "${YELLOW}Building echo-server image...${NC}"
    docker build -f docker/echo-server/Dockerfile -t echo-server:latest docker/echo-server/
    
    echo -e "${YELLOW}Loading images into kind cluster...${NC}"
    kind load docker-image ngx-inference:latest --name "$CLUSTER_NAME"
    kind load docker-image echo-server:latest --name "$CLUSTER_NAME"
    
    echo -e "${GREEN}✓ Images built and loaded${NC}"
    echo ""
}

# Deploy manifests
deploy_manifests() {
    echo -e "${YELLOW}Deploying base manifests...${NC}"
    
    # Create namespace
    kubectl apply -f "$TEST_DIR/manifests/01-namespace.yaml"
    
    echo -e "${GREEN}✓ Base manifests deployed${NC}"
    echo ""
}

# Deploy vLLM simulator and EPP
deploy_vllm_and_epp() {
    echo -e "${YELLOW}Deploying vLLM simulator and EPP...${NC}"
    
    # Install Gateway API Inference Extension CRDs first (required for InferencePool)
    echo -e "${YELLOW}Installing Gateway API Inference Extension CRDs...${NC}"
    kubectl apply -f https://github.com/kubernetes-sigs/gateway-api-inference-extension/releases/download/v1.1.0/manifests.yaml
    echo -e "${GREEN}✓ Gateway API Inference Extension CRDs installed${NC}"
    
    # Wait a moment for CRDs to be registered
    echo -e "${YELLOW}Waiting for CRDs to be registered...${NC}"
    sleep 3
    
    # Deploy vLLM simulator (backend pods) - must happen before InferencePool so EPP has something to discover
    echo -e "${YELLOW}Deploying vLLM simulator backend...${NC}"
    kubectl apply -f "$TEST_DIR/manifests/02-vllm-simulator.yaml"
    
    # Wait for simulator pods to be ready (should be quick)
    echo -e "${YELLOW}Waiting for vLLM simulator pods to be ready...${NC}"
    sleep 3
    if kubectl wait --for=condition=ready pod -l app=vllm-llama3-8b-instruct -n "$NAMESPACE" --timeout=120s 2>/dev/null; then
        echo -e "${GREEN}✓ vLLM simulator pods ready${NC}"
    else
        echo -e "${YELLOW}Warning: vLLM simulator pods may still be starting${NC}"
        echo -e "${YELLOW}Check status with: kubectl get pods -n $NAMESPACE -l app=vllm-llama3-8b-instruct${NC}"
    fi
    
    # Install EPP via Helm chart (this will also create the InferencePool)
    echo -e "${YELLOW}Installing EPP via InferencePool Helm chart...${NC}"
    helm install vllm-llama3-8b-instruct \
        --namespace "$NAMESPACE" \
        --create-namespace \
        --set inferencePool.modelServers.matchLabels.app=vllm-llama3-8b-instruct \
        --set inferenceExtension.flags[0].name=v \
        --set inferenceExtension.flags[0].value=4 \
        --set inferenceExtension.flags[1].name=secure-serving \
        --set inferenceExtension.flags[1].value=false \
        --set provider.name=none \
        oci://registry.k8s.io/gateway-api-inference-extension/charts/inferencepool \
        --version "$IGW_CHART_VERSION" \
        --wait \
        --timeout 5m
    echo -e "${GREEN}✓ EPP installed via Helm${NC}"
    
    # Wait for EPP pod to be ready
    echo -e "${YELLOW}Waiting for EPP pod to be ready...${NC}"
    sleep 3
    if kubectl wait --for=condition=ready pod -l inferencepool=vllm-llama3-8b-instruct-epp -n "$NAMESPACE" --timeout=120s 2>/dev/null; then
        echo -e "${GREEN}✓ EPP pod ready${NC}"
    else
        echo -e "${YELLOW}Warning: EPP pod may still be starting${NC}"
        echo -e "${YELLOW}Check status with: kubectl get pods -n $NAMESPACE${NC}"
    fi
    
    # Verify InferencePool was created
    echo -e "${YELLOW}Checking InferencePool resource...${NC}"
    if kubectl get inferencepool vllm-llama3-8b-instruct -n "$NAMESPACE" &>/dev/null; then
        echo -e "${GREEN}✓ InferencePool created${NC}"
    else
        echo -e "${YELLOW}Warning: InferencePool not found${NC}"
    fi
    
    # Wait for CoreDNS to be ready to ensure DNS resolution works
    echo -e "${YELLOW}Waiting for CoreDNS to be ready...${NC}"
    
    # Temporarily disable exit on error for CoreDNS check
    set +e
    local coredns_ready=false
    local wait_count=0
    
    # Wait for CoreDNS pods to exist (up to 30 seconds)
    while [ $wait_count -lt 30 ]; do
        local pod_check
        pod_check=$(kubectl get pods -n kube-system -l k8s-app=kube-dns --no-headers 2>/dev/null)
        if echo "$pod_check" | grep "coredns" >/dev/null 2>&1; then
            break
        fi
        sleep 1
        ((wait_count++))
    done
    
    # Now wait for CoreDNS to become ready
    local wait_output
    wait_output=$(kubectl wait --for=condition=ready pod -l k8s-app=kube-dns -n kube-system --timeout=60s 2>&1)
    if echo "$wait_output" | grep "condition met" >/dev/null 2>&1; then
        coredns_ready=true
    else
        wait_output=$(kubectl wait --for=condition=ready pod -l k8s-app=coredns -n kube-system --timeout=60s 2>&1)
        if echo "$wait_output" | grep "condition met" >/dev/null 2>&1; then
            coredns_ready=true
        fi
    fi
    
    # Re-enable exit on error
    set -e
    
    if [ "$coredns_ready" = true ]; then
        echo -e "${GREEN}✓ CoreDNS ready${NC}"
    else
        echo -e "${YELLOW}Warning: CoreDNS check timed out${NC}"
        echo -e "${YELLOW}Note: NGINX uses dynamic DNS resolution and can start without CoreDNS being ready${NC}"
    fi
    
    echo -e "${GREEN}✓ vLLM simulator and EPP deployed${NC}"
    echo ""
}

# Deploy echo-server and NGINX
deploy_nginx() {
    echo -e "${YELLOW}Deploying echo-server...${NC}"
    kubectl apply -f "$TEST_DIR/manifests/05-echo-server.yaml"
    
    # Wait for echo-server
    if kubectl wait --for=condition=ready pod -l app=echo-server -n "$NAMESPACE" --timeout=60s 2>/dev/null; then
        echo -e "${GREEN}✓ Echo-server ready${NC}"
    else
        echo -e "${YELLOW}Note: Echo-server may still be starting${NC}"
    fi
    
    echo -e "${YELLOW}Deploying NGINX with ngx-inference module...${NC}"
    
    kubectl apply -f "$TEST_DIR/manifests/04-nginx-inference.yaml"
    
    # Give Kubernetes time to schedule pods
    echo -e "${YELLOW}Waiting for NGINX pods to be scheduled...${NC}"
    sleep 3
    
    # Wait for NGINX to be ready (allow failure since pods may still be starting)
    if kubectl wait --for=condition=ready pod -l app=nginx-inference -n "$NAMESPACE" --timeout=60s 2>/dev/null; then
        echo -e "${GREEN}✓ NGINX pods ready${NC}"
    else
        echo -e "${YELLOW}Note: NGINX pods may still be starting${NC}"
        echo -e "${YELLOW}Check status with: kubectl get pods -n $NAMESPACE -l app=nginx-inference${NC}"
    fi
    
    echo -e "${GREEN}✓ NGINX deployed${NC}"
    echo ""
}

# Print access information
print_access_info() {
    echo -e "${GREEN}=== Setup Complete ===${NC}"
    echo ""
    echo "Cluster: $CLUSTER_NAME"
    echo "Namespace: $NAMESPACE"
    echo ""
    echo "Access NGINX via:"
    echo "  http://localhost:8080/health"
    echo "  http://localhost:8080/v1/chat/completions (EPP-enabled)"
    echo "  http://localhost:8080/v1/completions (direct to vLLM)"
    echo ""
    echo "Useful commands:"
    echo "  kubectl get pods -n $NAMESPACE"
    echo "  kubectl logs -n $NAMESPACE -l app=nginx-inference"
    echo "  kubectl logs -n $NAMESPACE -l app=vllm-llama3-8b-instruct"
    echo "  kubectl logs -n $NAMESPACE -l app.kubernetes.io/name=reference-epp"
    echo ""
    echo "Run tests with:"
    echo "  ./tests/kind-ngf/scripts/test.sh"
    echo ""
    echo "Clean up with:"
    echo "  kind delete cluster --name $CLUSTER_NAME"
    echo ""
}

# Main execution
main() {
    check_prerequisites
    create_cluster
    build_and_load_image
    deploy_manifests
    deploy_vllm_and_epp
    deploy_nginx
    print_access_info
}

main "$@"
