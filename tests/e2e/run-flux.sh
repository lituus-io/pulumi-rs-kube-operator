#!/usr/bin/env bash
# =============================================================================
# E2E Test: Full Stack lifecycle via Flux CD (create → update → destroy).
#
# Drives the operator through Flux Kustomizations pointing at the test repo:
#   1. CREATE: Flux deploys base overlay → Program + Stack → GCS bucket
#   2. UPDATE: Switch Kustomization path to update overlay → label change
#   3. DESTROY: Delete Kustomization (prune: true) → finalizer destroys bucket
#
# Usage:
#   ./tests/e2e/run-flux.sh
#   KEEP_CLUSTER=true ./tests/e2e/run-flux.sh
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CLUSTER_NAME="${CLUSTER_NAME:-pko-flux-test}"
NAMESPACE="${NAMESPACE:-pulumi-test}"
IMAGE_NAME="${IMAGE_NAME:-pulumi-rs-kube-operator:e2e}"
KEEP_CLUSTER="${KEEP_CLUSTER:-false}"
TIMEOUT="${TIMEOUT:-300}"
CREDS_FILE="${CREDS_FILE:-/Users/gatema/Desktop/drive/git/code/creds/terraform.json}"
TEST_REPO_URL="${TEST_REPO_URL:-https://github.com/terekete/pulumi-rs-kube-operator-test}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

pass=0
fail=0

log()  { echo -e "${GREEN}[FLUX-E2E]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[FAIL]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC} $*"; }

KFN="--context kind-$CLUSTER_NAME"

cleanup() {
    if [[ "$KEEP_CLUSTER" == "true" ]]; then
        warn "KEEP_CLUSTER=true — cluster '$CLUSTER_NAME' preserved"
        warn "  kubectl --context kind-$CLUSTER_NAME get stacks -A"
        warn "  kubectl --context kind-$CLUSTER_NAME get kustomizations -n flux-system"
        warn "  kind delete cluster --name $CLUSTER_NAME"
    else
        log "Cleaning up kind cluster '$CLUSTER_NAME'..."
        kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
    fi
}
trap cleanup EXIT

dump_debug_info() {
    warn "=== Operator logs ==="
    kubectl $KFN -n pulumi-system logs -l app.kubernetes.io/name=pulumi-operator --tail=100 2>/dev/null || true
    warn "=== Flux Kustomization status ==="
    kubectl $KFN -n flux-system get kustomizations -o wide 2>/dev/null || true
    warn "=== Flux GitRepository status ==="
    kubectl $KFN -n flux-system get gitrepositories -o wide 2>/dev/null || true
    warn "=== Stack describe ==="
    kubectl $KFN -n "$NAMESPACE" describe stack gcs-bucket-dev 2>/dev/null || true
    warn "=== Updates ==="
    kubectl $KFN -n "$NAMESPACE" get updates -o wide 2>/dev/null || true
    warn "=== Workspaces ==="
    kubectl $KFN -n "$NAMESPACE" get workspaces -o wide 2>/dev/null || true
    warn "=== Workspace pods ==="
    kubectl $KFN -n "$NAMESPACE" get pods -o wide 2>/dev/null || true
}

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

    # Resolve GitHub PAT for private repo access
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        GH_TOKEN="$GITHUB_TOKEN"
    elif command -v gh &>/dev/null; then
        GH_TOKEN="$(gh auth token 2>/dev/null || true)"
    fi
    if [[ -z "${GH_TOKEN:-}" ]]; then
        err "GitHub token required (set GITHUB_TOKEN or login via 'gh auth login')"
        exit 1
    fi
    log "Prerequisites OK"
}

install_flux_cli() {
    if command -v flux &>/dev/null; then
        log "Flux CLI already installed: $(flux --version)"
        return
    fi
    log "Installing Flux CLI..."
    curl -s https://fluxcd.io/install.sh | bash
    if ! command -v flux &>/dev/null; then
        err "Failed to install Flux CLI"
        exit 1
    fi
    log "Flux CLI installed: $(flux --version)"
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
    kubectl $KFN apply -f "$ROOT_DIR/deploy/crds/"
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

install_flux() {
    log "Installing Flux CD (source-controller + kustomize-controller)..."
    flux install \
        --context="kind-$CLUSTER_NAME" \
        --components=source-controller,kustomize-controller \
        --log-level=info \
        --timeout=120s
    log "Flux CD installed"

    # Wait for controllers to be ready
    kubectl $KFN -n flux-system wait deployment/source-controller \
        --for=condition=Available --timeout=60s
    kubectl $KFN -n flux-system wait deployment/kustomize-controller \
        --for=condition=Available --timeout=60s
    log "Flux controllers ready"
}

create_program_file_server_service() {
    log "Creating program file server service (port 9090)..."
    kubectl $KFN apply -f - <<EOF
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

    # Also set the env vars on the operator deployment so it knows its own service name
    kubectl $KFN -n pulumi-system set env deployment/pulumi-operator-pulumi-operator \
        OPERATOR_SERVICE_NAME=pulumi-operator \
        OPERATOR_NAMESPACE=pulumi-system 2>/dev/null || \
    kubectl $KFN -n pulumi-system set env deployment/pulumi-operator \
        OPERATOR_SERVICE_NAME=pulumi-operator \
        OPERATOR_NAMESPACE=pulumi-system 2>/dev/null || true

    # Wait for operator to restart with new env vars
    kubectl $KFN -n pulumi-system rollout status deployment -l app.kubernetes.io/name=pulumi-operator --timeout=60s || true
    log "Program file server service created"
}

setup_test_namespace() {
    log "Setting up test namespace '$NAMESPACE'..."
    kubectl $KFN create namespace "$NAMESPACE" --dry-run=client -o yaml \
        | kubectl $KFN apply -f -

    # Create GCP credentials secret
    kubectl $KFN -n "$NAMESPACE" create secret generic gcp-credentials \
        --from-file=credentials.json="$CREDS_FILE" \
        --dry-run=client -o yaml \
        | kubectl $KFN apply -f -

    # Create Pulumi passphrase secret (for file-based backend)
    kubectl $KFN -n "$NAMESPACE" create secret generic pulumi-passphrase \
        --from-literal=passPhrase="test" \
        --dry-run=client -o yaml \
        | kubectl $KFN apply -f -

    # Create ServiceAccount
    kubectl $KFN -n "$NAMESPACE" create serviceaccount pulumi-workspace \
        --dry-run=client -o yaml \
        | kubectl $KFN apply -f -

    log "Test namespace ready"
}

# ── Helpers ──────────────────────────────────────────────────────────────────

wait_for_condition() {
    local resource="$1" condition="$2" timeout="$3" ns="${4:-$NAMESPACE}"
    log "Waiting for $resource condition=$condition (timeout=${timeout}s)..."
    if kubectl $KFN -n "$ns" wait "$resource" \
        --for="condition=$condition" --timeout="${timeout}s" 2>/dev/null; then
        return 0
    fi
    return 1
}

get_stack_state() {
    kubectl $KFN -n "$NAMESPACE" get stack gcs-bucket-dev \
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

# ── Flux Resources ───────────────────────────────────────────────────────────

create_flux_git_repository() {
    log "Creating GitHub auth secret for Flux..."
    kubectl $KFN -n flux-system create secret generic github-auth \
        --from-literal=username=git \
        --from-literal=password="$GH_TOKEN" \
        --dry-run=client -o yaml \
        | kubectl $KFN apply -f -

    log "Creating Flux GitRepository source..."
    kubectl $KFN apply -f - <<EOF
apiVersion: source.toolkit.fluxcd.io/v1
kind: GitRepository
metadata:
  name: pulumi-test
  namespace: flux-system
spec:
  interval: 1m
  url: ${TEST_REPO_URL}
  ref:
    branch: main
  secretRef:
    name: github-auth
EOF
    # Wait for GitRepository to be ready
    if ! wait_for_condition "gitrepository/pulumi-test" "Ready" "120" "flux-system"; then
        err "GitRepository did not become Ready"
        kubectl $KFN -n flux-system describe gitrepository pulumi-test || true
        exit 1
    fi
    log "GitRepository ready"
}

create_flux_kustomization() {
    local path="$1"
    log "Creating Flux Kustomization (path=$path)..."
    kubectl $KFN apply -f - <<EOF
apiVersion: kustomize.toolkit.fluxcd.io/v1
kind: Kustomization
metadata:
  name: pulumi-test
  namespace: flux-system
spec:
  interval: 5m
  prune: true
  sourceRef:
    kind: GitRepository
    name: pulumi-test
  path: ${path}
  targetNamespace: ${NAMESPACE}
EOF
}

# ── Tests ────────────────────────────────────────────────────────────────────

test_create() {
    log "═══ TEST 1: CREATE via Flux Kustomization ═══"

    create_flux_kustomization "./infrastructure/base"

    # Wait for Flux Kustomization to reconcile
    if ! wait_for_condition "kustomization/pulumi-test" "Ready" "120" "flux-system"; then
        err "Flux Kustomization did not become Ready"
        dump_debug_info
        fail=$((fail + 1))
        return
    fi
    log "Flux Kustomization reconciled"

    # Verify Program CR exists
    if kubectl $KFN -n "$NAMESPACE" get program gcs-bucket &>/dev/null; then
        log "  PASS: Program CR exists"
        pass=$((pass + 1))
    else
        err "  FAIL: Program CR not found"
        fail=$((fail + 1))
    fi

    # Verify Stack CR exists
    if kubectl $KFN -n "$NAMESPACE" get stack gcs-bucket-dev &>/dev/null; then
        log "  PASS: Stack CR exists"
        pass=$((pass + 1))
    else
        err "  FAIL: Stack CR not found"
        fail=$((fail + 1))
        return
    fi

    # Wait for Stack to become Ready
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
    log "═══ TEST 2: UPDATE via Flux Kustomization path change ═══"

    # Patch the Kustomization to point to the update overlay
    kubectl $KFN -n flux-system patch kustomization pulumi-test \
        --type merge -p '{"spec":{"path":"./infrastructure/update"}}'

    # Force reconciliation
    flux reconcile kustomization pulumi-test -n flux-system --context="kind-$CLUSTER_NAME" || true

    # Wait for Kustomization to reconcile the new path
    sleep 5
    if ! wait_for_condition "kustomization/pulumi-test" "Ready" "120" "flux-system"; then
        err "Flux Kustomization did not become Ready after update"
        dump_debug_info
        fail=$((fail + 1))
        return
    fi
    log "Flux Kustomization reconciled with update overlay"

    # Wait for Stack to re-reconcile and become Ready again
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
    log "═══ TEST 3: DESTROY via Flux Kustomization deletion ═══"

    # Delete the Flux Kustomization — with prune: true, this removes Program + Stack
    kubectl $KFN -n flux-system delete kustomization pulumi-test --timeout="${TIMEOUT}s"
    log "Flux Kustomization deleted (prune will remove Stack + Program)"

    # Wait for Stack to be fully deleted (finalizer should run destroy)
    local elapsed=0
    local interval=5
    while kubectl $KFN -n "$NAMESPACE" get stack gcs-bucket-dev &>/dev/null; do
        if [[ $elapsed -ge $TIMEOUT ]]; then
            err "Stack still exists after ${TIMEOUT}s (finalizer may be stuck)"
            dump_debug_info
            fail=$((fail + 1))
            return
        fi
        info "Waiting for Stack deletion (${elapsed}s/${TIMEOUT}s)..."
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done

    log "  PASS: Stack deleted (finalizer ran destroy)"
    pass=$((pass + 1))

    # Verify Program is also deleted
    if ! kubectl $KFN -n "$NAMESPACE" get program gcs-bucket &>/dev/null; then
        log "  PASS: Program CR also deleted"
        pass=$((pass + 1))
    else
        warn "  Program CR still exists (may have finalizer pending)"
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────────

main() {
    log "Starting Flux CD E2E lifecycle tests..."
    log "Test repo: $TEST_REPO_URL"

    # Setup
    check_prereqs
    install_flux_cli
    create_cluster
    build_and_load_image
    install_crds
    install_operator
    create_program_file_server_service
    install_flux
    setup_test_namespace

    # Create Flux GitRepository source
    create_flux_git_repository

    # Tests
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
