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
IGW_CHART_VERSION="${IGW_CHART_VERSION:-v1.2.0}"

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$(dirname "$TEST_DIR")")"

echo -e "${GREEN}=== NGX-Inference Reference EPP Test Setup ===${NC}"
echo "Project root: $PROJECT_ROOT"
echo "Test directory: $TEST_DIR"
echo ""

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

# Wait for all nodes to be ready
wait_for_nodes() {
    echo -e "${YELLOW}Waiting for all nodes to be ready...${NC}"

    # Get expected number of nodes from kind config
    local expected_nodes=$(grep -c "role:" "$TEST_DIR/cluster/kind-config.yaml")
    echo "Expected nodes: $expected_nodes"

    # Wait for all nodes to exist
    echo -e "${YELLOW}Waiting for $expected_nodes nodes to be registered...${NC}"
    local timeout=120
    local count=0
    while [ $count -lt $timeout ]; do
        local current_nodes=$(kubectl get nodes --no-headers 2>/dev/null | wc -l)
        if [ "$current_nodes" -eq "$expected_nodes" ]; then
            break
        fi
        echo "Nodes registered: $current_nodes/$expected_nodes"
        sleep 2
        ((count += 2))
    done

    if [ $count -ge $timeout ]; then
        echo -e "${RED}Timeout waiting for nodes to be registered${NC}"
        exit 1
    fi

    # Wait for all nodes to be ready
    echo -e "${YELLOW}Waiting for all $expected_nodes nodes to be ready...${NC}"
    if ! kubectl wait --for=condition=ready nodes --all --timeout=300s; then
        echo -e "${RED}Timeout waiting for nodes to be ready${NC}"
        echo "Current node status:"
        kubectl get nodes
        exit 1
    fi

    echo -e "${GREEN}✓ All $expected_nodes nodes are ready${NC}"

    # Show node distribution for verification
    echo "Node status:"
    kubectl get nodes -o wide
    echo ""
}

# Build and load images
build_and_load_image() {
    echo -e "${YELLOW}Building NGINX image with ngx-inference module...${NC}"

    cd "$PROJECT_ROOT"
    docker build -f docker/nginx/Dockerfile -t ngx-inference:latest .

    echo -e "${YELLOW}Loading images into kind cluster...${NC}"
    kind load docker-image ngx-inference:latest --name "$CLUSTER_NAME"

    echo -e "${GREEN}✓ Images built and loaded${NC}"
    echo ""
}

# Generate TLS certificate and create Kubernetes secret
generate_tls_certificate() {
    echo -e "${YELLOW}Generating TLS certificate for EPP...${NC}"

    # Create temporary directory for certificates
    local cert_dir=$(mktemp -d)
    local cert_file="$cert_dir/tls.crt"
    local key_file="$cert_dir/tls.key"

    # Generate private key
    openssl genrsa -out "$key_file" 2048

    # Generate self-signed certificate
    # Using SAN for better compatibility with modern TLS clients
    openssl req -new -x509 -key "$key_file" -out "$cert_file" -days 1 \
        -subj "/C=US/ST=CA/L=TestCity/O=TestOrg/OU=TestUnit/CN=vllm-llama3-8b-instruct-epp.${NAMESPACE}.svc.cluster.local" \
        -addext "subjectAltName=DNS:vllm-llama3-8b-instruct-epp.${NAMESPACE}.svc.cluster.local,DNS:vllm-llama3-8b-instruct-epp,DNS:localhost,IP:127.0.0.1" \
        -addext "basicConstraints=CA:FALSE" \
        -addext "keyUsage=digitalSignature,keyEncipherment" \
        -addext "extendedKeyUsage=serverAuth"

    # Create Kubernetes secret
    kubectl create secret tls epp-tls-secret \
        --cert="$cert_file" \
        --key="$key_file" \
        --namespace="$NAMESPACE"

    # Clean up temporary files
    rm -rf "$cert_dir"

    echo -e "${GREEN}✓ TLS certificate generated and secret created${NC}"
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
    echo -e "${YELLOW}Installing EPP via InferencePool Helm chart with TLS enabled...${NC}"
    helm install vllm-llama3-8b-instruct \
        --namespace "$NAMESPACE" \
        --create-namespace \
        --set inferencePool.modelServers.matchLabels.app=vllm-llama3-8b-instruct \
        --set inferenceExtension.flags.v=4 \
        --set inferenceExtension.flags.cert-path=/etc/tls \
        --set inferenceExtension.flags.secure-serving=true \
        --set provider.name=none \
        oci://registry.k8s.io/gateway-api-inference-extension/charts/inferencepool \
        --version "$IGW_CHART_VERSION"
    echo -e "${GREEN}✓ EPP helm chart installed${NC}"

    # Immediately patch the deployment to add TLS volume mounts before EPP starts
    echo -e "${YELLOW}Patching EPP deployment to mount TLS certificate...${NC}"
    kubectl patch deployment vllm-llama3-8b-instruct-epp -n "$NAMESPACE" --type='json' -p='[
        {"op": "add", "path": "/spec/template/spec/volumes/-", "value": {"name": "epp-tls", "secret": {"secretName": "epp-tls-secret"}}},
        {"op": "add", "path": "/spec/template/spec/containers/0/volumeMounts/-", "value": {"name": "epp-tls", "mountPath": "/etc/tls", "readOnly": true}}
    ]'
    echo -e "${GREEN}✓ EPP deployment patched with TLS volume${NC}"

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

# Deploy NGINX
deploy_nginx() {
    echo -e "${YELLOW}Deploying NGINX with ngx-inference module...${NC}"

    # Generate initial nginx config using generate-config.sh with bbr_on_epp_on scenario
    echo -e "${YELLOW}Generating initial nginx configuration...${NC}"
    local tmp_config="/tmp/nginx-kind-initial.conf"
    "$PROJECT_ROOT/tests/generate-config.sh" \
        -e kind \
        -o "$tmp_config" \
        -s "bbr_on_epp_on" \
        -n "$NAMESPACE"

    # Create initial ConfigMap (replace underscores with hyphens for k8s naming)
    echo -e "${YELLOW}Creating initial ConfigMap...${NC}"
    kubectl create configmap nginx-inference-bbr-on-epp-on \
        --from-file=nginx.conf="$tmp_config" \
        -n "$NAMESPACE"
    rm -f "$tmp_config"

    # Deploy Service and Deployment from manifest
    echo -e "${YELLOW}Deploying NGINX Service and Deployment...${NC}"
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
    echo "EPP Configuration:"
    echo "  TLS: ENABLED with self-signed certificate"
    echo "  Certificate mounted at: /etc/tls/"
    echo ""
    echo "Access NGINX directly via NodePort (with kind port mapping):"
    echo "  http://localhost:30080/health"
    echo "  http://localhost:30080/v1/chat/completions (EPP-enabled with TLS)"
    echo ""
    echo "Useful commands:"
    echo "  kubectl get pods -n $NAMESPACE"
    echo "  kubectl logs -n $NAMESPACE -l app=nginx-inference"
    echo "  kubectl logs -n $NAMESPACE -l app=vllm-llama3-8b-instruct"
    echo "  kubectl logs -n $NAMESPACE -l app.kubernetes.io/name=reference-epp"
    echo "  kubectl describe secret epp-tls-secret -n $NAMESPACE  # Check TLS certificate"
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
    create_cluster
    build_and_load_image
    wait_for_nodes
    deploy_manifests
    generate_tls_certificate
    deploy_vllm_and_epp
    deploy_nginx
    print_access_info
}

main "$@"
