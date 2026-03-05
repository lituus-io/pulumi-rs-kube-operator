#!/usr/bin/env bash
# =============================================================================
# E2E Test: Full Stack lifecycle (create → update → destroy) on a kind cluster.
#
# Usage:
#   ./tests/e2e/run.sh              # full run, cluster cleaned up
#   KEEP_CLUSTER=true ./tests/e2e/run.sh   # keep cluster for debugging
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CLUSTER_NAME="${CLUSTER_NAME:-pko-test}"
NAMESPACE="${NAMESPACE:-pulumi-test}"
IMAGE_NAME="${IMAGE_NAME:-pulumi-rs-kube-operator:e2e}"
KEEP_CLUSTER="${KEEP_CLUSTER:-false}"
TIMEOUT="${TIMEOUT:-300}"
CREDS_FILE="${CREDS_FILE:-/Users/gatema/Desktop/drive/git/code/creds/terraform.json}"
TEST_REPO="${TEST_REPO:-/Users/gatema/Desktop/test/pulumi-rs-kube-operator-test}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass=0
fail=0

log()  { echo -e "${GREEN}[E2E]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[FAIL]${NC} $*"; }

cleanup() {
    if [[ "$KEEP_CLUSTER" == "true" ]]; then
        warn "KEEP_CLUSTER=true — cluster '$CLUSTER_NAME' preserved"
        warn "  kubectl --context kind-$CLUSTER_NAME get stacks -A"
        warn "  kind delete cluster --name $CLUSTER_NAME"
    else
        log "Cleaning up kind cluster '$CLUSTER_NAME'..."
        kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────────────

check_prereqs() {
    local missing=()
    for cmd in kind kubectl helm docker; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        err "Missing prerequisites: ${missing[*]}"
        exit 1
    fi
    if [[ ! -f "$CREDS_FILE" ]]; then
        err "Credentials file not found: $CREDS_FILE"
        exit 1
    fi
    if [[ ! -d "$TEST_REPO" ]]; then
        err "Test repo not found: $TEST_REPO"
        exit 1
    fi
    log "Prerequisites OK"
}

# ── Cluster Setup ────────────────────────────────────────────────────────────

create_cluster() {
    if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
        log "Kind cluster '$CLUSTER_NAME' already exists, reusing"
        return
    fi
    log "Creating kind cluster '$CLUSTER_NAME'..."
    kind create cluster --name "$CLUSTER_NAME" --wait 60s
    log "Cluster created"
}

build_and_load_image() {
    log "Building operator Docker image ($IMAGE_NAME)..."
    docker build -t "$IMAGE_NAME" -f "$ROOT_DIR/Dockerfile" "$ROOT_DIR"
    log "Loading image into kind..."
    kind load docker-image "$IMAGE_NAME" --name "$CLUSTER_NAME"
    log "Image loaded"
}

install_crds() {
    log "Installing CRDs..."
    kubectl --context "kind-$CLUSTER_NAME" apply -f "$ROOT_DIR/deploy/crds/"
    log "CRDs installed"
}

install_operator() {
    log "Installing operator via Helm..."
    helm upgrade --install pulumi-operator "$ROOT_DIR/deploy/helm/pulumi-operator" \
        --kube-context "kind-$CLUSTER_NAME" \
        --namespace pulumi-system --create-namespace \
        --set image.registry="" \
        --set image.repository="pulumi-rs-kube-operator" \
        --set image.tag="e2e" \
        --set image.pullPolicy=Never \
        --set operator.logLevel=debug \
        --wait --timeout 120s
    log "Operator installed"
}

create_program_file_server_service() {
    log "Creating program file server service (port 9090)..."
    kubectl --context "kind-$CLUSTER_NAME" apply -f - <<EOF
apiVersion: v1
kind: Service
metadata:
  name: pulumi-operator
  namespace: pulumi-system
spec:
  type: ClusterIP
  ports:
    - port: 9090
      targetPort: 9090
      protocol: TCP
      name: file-server
  selector:
    app.kubernetes.io/name: pulumi-operator
    app.kubernetes.io/instance: pulumi-operator
EOF
    kubectl --context "kind-$CLUSTER_NAME" -n pulumi-system set env deployment/pulumi-operator-pulumi-operator \
        OPERATOR_SERVICE_NAME=pulumi-operator \
        OPERATOR_NAMESPACE=pulumi-system 2>/dev/null || \
    kubectl --context "kind-$CLUSTER_NAME" -n pulumi-system set env deployment/pulumi-operator \
        OPERATOR_SERVICE_NAME=pulumi-operator \
        OPERATOR_NAMESPACE=pulumi-system 2>/dev/null || true
    kubectl --context "kind-$CLUSTER_NAME" -n pulumi-system rollout status deployment -l app.kubernetes.io/name=pulumi-operator --timeout=60s || true
    log "Program file server service created"
}

setup_test_namespace() {
    log "Setting up test namespace '$NAMESPACE'..."
    kubectl --context "kind-$CLUSTER_NAME" create namespace "$NAMESPACE" --dry-run=client -o yaml \
        | kubectl --context "kind-$CLUSTER_NAME" apply -f -

    # Create GCP credentials secret
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" create secret generic gcp-credentials \
        --from-file=credentials.json="$CREDS_FILE" \
        --dry-run=client -o yaml \
        | kubectl --context "kind-$CLUSTER_NAME" apply -f -

    # Create ServiceAccount
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" create serviceaccount pulumi-workspace \
        --dry-run=client -o yaml \
        | kubectl --context "kind-$CLUSTER_NAME" apply -f -

    log "Test namespace ready"
}

# ── Helpers ──────────────────────────────────────────────────────────────────

dump_debug_info() {
    warn "=== Operator logs ==="
    kubectl --context "kind-$CLUSTER_NAME" -n pulumi-system logs -l app.kubernetes.io/name=pulumi-operator --tail=100 || true
    warn "=== Stack describe ==="
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" describe stack gcs-bucket-dev || true
    warn "=== Updates ==="
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" get updates -o wide || true
    warn "=== Workspaces ==="
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" get workspaces -o wide || true
    warn "=== Workspace pods ==="
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" get pods -o wide || true
}

wait_for_condition() {
    local resource="$1" condition="$2" timeout="$3"
    log "Waiting for $resource condition=$condition (timeout=${timeout}s)..."
    if kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" wait "$resource" \
        --for="condition=$condition" --timeout="${timeout}s" 2>/dev/null; then
        return 0
    fi
    return 1
}

get_stack_state() {
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" get stack gcs-bucket-dev \
        -o jsonpath='{.status.lastUpdate.state}' 2>/dev/null || echo ""
}

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        log "  PASS: $desc (=$expected)"
        pass=$((pass + 1))
    else
        err "  FAIL: $desc — expected='$expected', got='$actual'"
        fail=$((fail + 1))
    fi
}

# ── Tests ────────────────────────────────────────────────────────────────────

test_create() {
    log "═══ TEST: CREATE Stack ═══"
    kubectl --context "kind-$CLUSTER_NAME" apply -k "$TEST_REPO/infrastructure/base/"

    if wait_for_condition "stack/gcs-bucket-dev" "Ready" "$TIMEOUT"; then
        local state
        state=$(get_stack_state)
        assert_eq "Stack state after create" "succeeded" "$state"
    else
        err "Stack did not become Ready within ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 1))
    fi
}

test_update() {
    log "═══ TEST: UPDATE Stack (change region) ═══"
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" patch stack gcs-bucket-dev \
        --type merge -p '{"spec":{"config":{"gcp:region":"us-east1"}}}'

    # Wait for a new reconcile — the Ready condition will be reset
    sleep 5
    if wait_for_condition "stack/gcs-bucket-dev" "Ready" "$TIMEOUT"; then
        local state
        state=$(get_stack_state)
        assert_eq "Stack state after update" "succeeded" "$state"
    else
        err "Stack did not become Ready after update within ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 1))
    fi
}

test_destroy() {
    log "═══ TEST: DESTROY Stack (delete CR) ═══"
    kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" delete stack gcs-bucket-dev \
        --timeout="${TIMEOUT}s"

    # Verify the stack is gone
    if ! kubectl --context "kind-$CLUSTER_NAME" -n "$NAMESPACE" get stack gcs-bucket-dev &>/dev/null; then
        log "  PASS: Stack deleted (finalizer ran destroy)"
        pass=$((pass + 1))
    else
        err "  FAIL: Stack still exists after delete"
        fail=$((fail + 1))
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────────

main() {
    log "Starting E2E tests..."
    check_prereqs
    create_cluster
    build_and_load_image
    install_crds
    install_operator
    create_program_file_server_service
    setup_test_namespace

    test_create
    test_update
    test_destroy

    echo ""
    log "══════════════════════════════════════"
    log "Results: ${pass} passed, ${fail} failed"
    log "══════════════════════════════════════"

    if [[ $fail -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
