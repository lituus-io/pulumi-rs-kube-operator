#!/usr/bin/env bash
# =============================================================================
# E2E Test: 10 concurrent GCS buckets via Flux CD (create → update → destroy).
#
# Tests operator concurrency with 10 independent Flux Kustomizations, each
# deploying a separate GCS bucket. Verifies:
#   - 10 concurrent creates  (all succeed, workspaces scale to zero)
#   - 10 concurrent updates  (label change dev→staging, workspaces scale to zero)
#   - 10 concurrent destroys (Kustomization deletion → finalizer destroys)
#   - GCS state backend (gs://pulumi-state-pko-test)
#   - Ephemeral workspaces (workspaceReclaimPolicy: Delete)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CLUSTER_NAME="${CLUSTER_NAME:-pko-flux-concurrent}"
NAMESPACE="${NAMESPACE:-pulumi-test}"
IMAGE_NAME="${IMAGE_NAME:-pulumi-rs-kube-operator:e2e}"
KEEP_CLUSTER="${KEEP_CLUSTER:-false}"
TIMEOUT="${TIMEOUT:-600}"
BUCKET_COUNT=10
CREDS_FILE="${CREDS_FILE:-/Users/gatema/Desktop/drive/git/code/creds/terraform.json}"
TEST_REPO_URL="${TEST_REPO_URL:-https://github.com/terekete/pulumi-rs-kube-operator-test}"
GCLOUD="${GCLOUD:-/Users/gatema/google-cloud-sdk/bin/gcloud}"
GCP_PROJECT="${GCP_PROJECT:-spacy-muffin-lab-5a292e}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

pass=0
fail=0

log()  { echo -e "${GREEN}[CONCURRENT-E2E]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[FAIL]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC} $*"; }

KFN="--context kind-$CLUSTER_NAME"

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

dump_debug_info() {
    warn "=== Operator logs (last 50) ==="
    kubectl $KFN -n pulumi-system logs -l app.kubernetes.io/name=pulumi-operator --tail=50 2>/dev/null || true
    warn "=== Stacks ==="
    kubectl $KFN -n "$NAMESPACE" get stacks -o wide 2>/dev/null || true
    warn "=== Updates ==="
    kubectl $KFN -n "$NAMESPACE" get updates -o wide 2>/dev/null || true
    warn "=== Workspaces ==="
    kubectl $KFN -n "$NAMESPACE" get workspaces -o wide 2>/dev/null || true
    warn "=== Pods ==="
    kubectl $KFN -n "$NAMESPACE" get pods -o wide 2>/dev/null || true
}

# ── Dashboard ─────────────────────────────────────────────────────────────────

show_progress() {
    local phase="$1"
    local start_time="$2"
    local elapsed=$(( $(date +%s) - start_time ))

    echo -e "\n${BOLD}═══ ${phase} Progress (${elapsed}s) ═══${NC}"

    # Stacks
    local ready=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
        python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if any(c.get('type')=='Ready' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[]))))" 2>/dev/null || echo 0)
    local total=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')
    echo -e "  Stacks Ready:    ${GREEN}${ready}${NC}/${total}"

    # Workspaces (pods running)
    local ws_pods=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | { grep -c "Running" || true; })
    echo -e "  Workspace Pods:  ${CYAN}${ws_pods}${NC}"

    # Updates
    local complete=$(kubectl $KFN -n "$NAMESPACE" get updates -o json 2>/dev/null | \
        python3 -c "import json,sys; d=json.load(sys.stdin); items=d.get('items',[]); print(f\"{sum(1 for i in items if any(c.get('type')=='Complete' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[])))}/{len(items)}\")" 2>/dev/null || echo "?/?")
    echo -e "  Updates Done:    ${GREEN}${complete}${NC}"

    echo ""
}

# ── Prerequisites ─────────────────────────────────────────────────────────────

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
        log "Flux CLI: $(flux --version)"
        return
    fi
    curl -s https://fluxcd.io/install.sh | bash
}

# ── Cluster Setup ─────────────────────────────────────────────────────────────

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
    log "Installing Flux CD..."
    flux install \
        --context="kind-$CLUSTER_NAME" \
        --components=source-controller,kustomize-controller \
        --log-level=info \
        --timeout=120s
    kubectl $KFN -n flux-system wait deployment/source-controller --for=condition=Available --timeout=60s
    kubectl $KFN -n flux-system wait deployment/kustomize-controller --for=condition=Available --timeout=60s
    log "Flux CD installed and ready"
}

create_program_file_server_service() {
    log "Creating program file server service..."
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
    kubectl $KFN -n pulumi-system set env deployment/pulumi-operator \
        OPERATOR_SERVICE_NAME=pulumi-operator \
        OPERATOR_NAMESPACE=pulumi-system 2>/dev/null || true
    kubectl $KFN -n pulumi-system rollout status deployment -l app.kubernetes.io/name=pulumi-operator --timeout=60s || true
    log "File server service ready"
}

setup_test_namespace() {
    log "Setting up test namespace '$NAMESPACE'..."
    kubectl $KFN create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl $KFN apply -f -
    kubectl $KFN -n "$NAMESPACE" create secret generic gcp-credentials \
        --from-file=credentials.json="$CREDS_FILE" \
        --dry-run=client -o yaml | kubectl $KFN apply -f -
    kubectl $KFN -n "$NAMESPACE" create secret generic pulumi-passphrase \
        --from-literal=passPhrase="pulumi-operator-test" \
        --dry-run=client -o yaml | kubectl $KFN apply -f -
    kubectl $KFN -n "$NAMESPACE" create serviceaccount pulumi-workspace \
        --dry-run=client -o yaml | kubectl $KFN apply -f -
    log "Test namespace ready"
}

# ── Flux Resources ────────────────────────────────────────────────────────────

create_flux_git_repository() {
    log "Creating Flux GitRepository source..."
    kubectl $KFN -n flux-system create secret generic github-auth \
        --from-literal=username=git \
        --from-literal=password="$GH_TOKEN" \
        --dry-run=client -o yaml | kubectl $KFN apply -f -
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
    kubectl $KFN -n flux-system wait gitrepository/pulumi-test --for=condition=Ready --timeout=120s
    log "GitRepository ready"
}

create_kustomization() {
    local name="$1" path="$2"
    kubectl $KFN apply -f - <<EOF
apiVersion: kustomize.toolkit.fluxcd.io/v1
kind: Kustomization
metadata:
  name: ${name}
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

# ── Helpers ───────────────────────────────────────────────────────────────────

wait_all_stacks_ready() {
    local timeout="$1"
    local start_time=$(date +%s)
    local elapsed=0

    while [[ $elapsed -lt $timeout ]]; do
        show_progress "$2" "$start_time"

        local ready_count=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if any(c.get('type')=='Ready' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[]))))" 2>/dev/null || echo 0)
        local total=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')

        if [[ "$ready_count" -ge "$BUCKET_COUNT" && "$total" -ge "$BUCKET_COUNT" ]]; then
            return 0
        fi

        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_all_stacks_deleted() {
    local timeout="$1"
    local start_time=$(date +%s)
    local elapsed=0

    while [[ $elapsed -lt $timeout ]]; do
        local count=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')
        info "Stacks remaining: $count (${elapsed}s/${timeout}s)"

        if [[ "$count" -eq 0 ]]; then
            return 0
        fi

        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_workspaces_zero() {
    local timeout="$1"
    local start_time=$(date +%s)
    local elapsed=0

    while [[ $elapsed -lt $timeout ]]; do
        local ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | { grep -c "workspace" || true; })
        if [[ "$ws_count" -eq 0 ]]; then
            return 0
        fi
        info "Workspace pods still running: $ws_count (${elapsed}s)"
        sleep 5
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

count_gcs_buckets() {
    GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage ls --project "$GCP_PROJECT" 2>/dev/null | { grep -c "bucket-" || true; }
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

assert_ge() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$actual" -ge "$expected" ]]; then
        log "  PASS: $desc ($actual >= $expected)"
        pass=$((pass + 1))
    else
        err "  FAIL: $desc — expected >= $expected, got='$actual'"
        fail=$((fail + 1))
    fi
}

# ── Tests ─────────────────────────────────────────────────────────────────────

test_concurrent_create() {
    log "═══ TEST 1: CONCURRENT CREATE (10 buckets) ═══"

    # Create all 10 kustomizations simultaneously
    for i in $(seq -w 1 $BUCKET_COUNT); do
        create_kustomization "bucket-${i}" "./infrastructure/bucket-${i}"
    done
    log "All $BUCKET_COUNT Kustomizations created"

    # Wait for all Flux Kustomizations to reconcile
    for i in $(seq -w 1 $BUCKET_COUNT); do
        kubectl $KFN -n flux-system wait kustomization/bucket-${i} --for=condition=Ready --timeout=120s 2>/dev/null || \
            warn "Kustomization bucket-${i} not Ready yet"
    done

    # Wait for all Stacks to become Ready
    if wait_all_stacks_ready "$TIMEOUT" "CREATE"; then
        local ready_count=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if any(c.get('type')=='Ready' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[]))))" 2>/dev/null || echo 0)
        assert_eq "All stacks Ready after create" "$BUCKET_COUNT" "$ready_count"

        # Verify all states are succeeded
        local succeeded=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if i.get('status',{}).get('lastUpdate',{}).get('state')=='succeeded'))" 2>/dev/null || echo 0)
        assert_eq "All stacks succeeded" "$BUCKET_COUNT" "$succeeded"
    else
        err "Not all stacks became Ready within ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 2))
        return
    fi

    # Verify workspace pods scale to zero (ephemeral)
    log "Checking workspaces scale to zero..."
    if wait_workspaces_zero 120; then
        log "  PASS: All workspaces scaled to zero"
        pass=$((pass + 1))
    else
        local ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | { grep -c "workspace" || true; })
        err "  FAIL: $ws_count workspace pods still running after create"
        fail=$((fail + 1))
    fi

    # Verify GCS buckets exist
    local bucket_count=$(count_gcs_buckets)
    assert_ge "GCS buckets created on GCP" "$BUCKET_COUNT" "$bucket_count"
}

test_concurrent_update() {
    log "═══ TEST 2: CONCURRENT UPDATE (10 buckets, label change) ═══"

    # Patch all kustomizations to point to update overlay
    for i in $(seq -w 1 $BUCKET_COUNT); do
        kubectl $KFN -n flux-system patch kustomization bucket-${i} \
            --type merge -p "{\"spec\":{\"path\":\"./infrastructure/bucket-${i}-update\"}}"
    done
    log "All $BUCKET_COUNT Kustomizations patched to update overlay"

    # Force reconciliation
    for i in $(seq -w 1 $BUCKET_COUNT); do
        flux reconcile kustomization bucket-${i} -n flux-system --context="kind-$CLUSTER_NAME" 2>/dev/null || true
    done

    # Wait for stacks to re-reconcile
    sleep 10
    if wait_all_stacks_ready "$TIMEOUT" "UPDATE"; then
        local succeeded=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if i.get('status',{}).get('lastUpdate',{}).get('state')=='succeeded'))" 2>/dev/null || echo 0)
        assert_eq "All stacks succeeded after update" "$BUCKET_COUNT" "$succeeded"
    else
        err "Not all stacks became Ready after update within ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 1))
        return
    fi

    # Verify workspaces scale to zero again
    log "Checking workspaces scale to zero after update..."
    if wait_workspaces_zero 120; then
        log "  PASS: All workspaces scaled to zero after update"
        pass=$((pass + 1))
    else
        local ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | { grep -c "workspace" || true; })
        err "  FAIL: $ws_count workspace pods still running after update"
        fail=$((fail + 1))
    fi
}

test_concurrent_destroy() {
    log "═══ TEST 3: CONCURRENT DESTROY (10 buckets) ═══"

    # Delete all Kustomizations simultaneously (prune: true removes Stack+Program)
    for i in $(seq -w 1 $BUCKET_COUNT); do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done
    log "All $BUCKET_COUNT Kustomizations deleted"

    # Wait for all stacks to be fully deleted
    if wait_all_stacks_deleted "$TIMEOUT"; then
        log "  PASS: All stacks deleted (finalizers ran destroy)"
        pass=$((pass + 1))
    else
        local remaining=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')
        err "  FAIL: $remaining stacks still exist after ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 1))
    fi

    # Verify GCS buckets are destroyed
    sleep 5
    local bucket_count=$(count_gcs_buckets)
    assert_eq "GCS buckets remaining on GCP" "0" "$bucket_count"

    # Verify no workspace pods
    local ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | { grep -c "workspace" || true; })
    assert_eq "Workspace pods after destroy" "0" "$ws_count"
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
    log "Starting 10-concurrent-bucket Flux CD E2E lifecycle tests..."
    log "Test repo: $TEST_REPO_URL"
    log "GCS state backend: gs://pulumi-state-pko-test"
    log "Workspace policy: Delete (ephemeral)"

    check_prereqs
    install_flux_cli
    create_cluster
    build_and_load_image
    install_crds
    install_operator
    create_program_file_server_service
    install_flux
    setup_test_namespace
    create_flux_git_repository

    test_concurrent_create
    test_concurrent_update
    test_concurrent_destroy

    echo ""
    log "══════════════════════════════════════"
    log "Results: ${pass} passed, ${fail} failed"
    log "══════════════════════════════════════"

    if [[ $fail -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
