#!/usr/bin/env bash
# =============================================================================
# E2E Test: Mixed concurrent lifecycle operations via Flux CD.
#
# Tests operator robustness with overlapping lifecycle events:
#   Phase 1: CREATE all 10 buckets concurrently
#   Phase 2: Simultaneously UPDATE buckets 01-05 AND DELETE buckets 06-10
#   Phase 3: DELETE remaining buckets 01-05 (after update completes)
#   Phase 4: Verify GCP resources are fully cleaned up
#
# This exercises the operator handling creates, updates, and destroys
# all running at the same time across different stacks.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CLUSTER_NAME="${CLUSTER_NAME:-pko-flux-mixed}"
NAMESPACE="${NAMESPACE:-pulumi-test}"
IMAGE_NAME="${IMAGE_NAME:-pulumi-rs-kube-operator:e2e}"
KEEP_CLUSTER="${KEEP_CLUSTER:-false}"
TIMEOUT="${TIMEOUT:-600}"
BUCKET_COUNT=10
CREDS_FILE="${CREDS_FILE:-/Users/gatema/Desktop/drive/git/code/creds/terraform.json}"
TEST_REPO_URL="${TEST_REPO_URL:-https://github.com/terekete/pulumi-rs-kube-operator-test}"
GCLOUD="${GCLOUD:-$HOME/google-cloud-sdk/bin/gcloud}"
GCP_PROJECT="${GCP_PROJECT:-spacy-muffin-lab-5a292e}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

pass=0
fail=0
declare -A BUCKET_NAMES  # Map of bucket index (01-10) → GCS bucket name

log()  { echo -e "${GREEN}[MIXED-E2E]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[FAIL]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC} $*"; }

KFN="--context kind-$CLUSTER_NAME"

cleanup() {
    if [[ "$KEEP_CLUSTER" == "true" ]]; then
        warn "KEEP_CLUSTER=true -- cluster '$CLUSTER_NAME' preserved"
        warn "  kubectl --context kind-$CLUSTER_NAME get stacks -A"
        warn "  kind delete cluster --name $CLUSTER_NAME"
    else
        log "Cleaning up kind cluster '$CLUSTER_NAME'..."
        kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
    fi
}
trap cleanup EXIT

dump_debug_info() {
    warn "=== Operator logs (last 80) ==="
    kubectl $KFN -n pulumi-system logs -l app.kubernetes.io/name=pulumi-operator --tail=80 2>/dev/null | grep -v '"level":"DEBUG"' || true
    warn "=== Stacks ==="
    kubectl $KFN -n "$NAMESPACE" get stacks -o wide 2>/dev/null || true
    warn "=== Updates ==="
    kubectl $KFN -n "$NAMESPACE" get updates -o wide 2>/dev/null || true
    warn "=== Pods ==="
    kubectl $KFN -n "$NAMESPACE" get pods -o wide 2>/dev/null || true
}

# ── Dashboard ─────────────────────────────────────────────────────────────────

show_mixed_progress() {
    local phase="$1"
    local start_time="$2"
    local elapsed=$(( $(date +%s) - start_time ))

    echo -e "\n${BOLD}--- ${phase} (${elapsed}s) ---${NC}"

    # Per-stack status
    kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
        python3 -c "
import json, sys
d = json.load(sys.stdin)
for item in sorted(d.get('items', []), key=lambda x: x['metadata']['name']):
    name = item['metadata']['name']
    conds = item.get('status', {}).get('conditions', [])
    ready = any(c['type'] == 'Ready' and c['status'] == 'True' for c in conds)
    reconciling = any(c['type'] == 'Reconciling' and c['status'] == 'True' for c in conds)
    last = item.get('status', {}).get('lastUpdate', {})
    state = last.get('state', 'pending')
    utype = last.get('type', '?')
    deleting = item['metadata'].get('deletionTimestamp') is not None
    status = 'DELETING' if deleting else ('READY' if ready else ('RECONCILING' if reconciling else 'WAITING'))
    print(f'  {name:30s} {status:12s} last={utype}/{state}')
" 2>/dev/null || echo "  (no stacks)"

    # Pod count
    local pods=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
    echo -e "  Workspace pods: ${CYAN}${pods}${NC}"
    echo ""
}

# ── Prerequisites ─────────────────────────────────────────────────────────────

check_prereqs() {
    local missing=()
    for cmd in kind kubectl helm docker flux; do
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

wait_stacks_ready() {
    local timeout="$1" target="$2" phase="$3"
    local start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        show_mixed_progress "$phase" "$start_time"
        local ready_count=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if any(c.get('type')=='Ready' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[]))))" 2>/dev/null || echo 0)
        if [[ "$ready_count" -ge "$target" ]]; then
            return 0
        fi
        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_specific_stacks_deleted() {
    local timeout="$1"
    shift
    local names=("$@")
    local start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        local remaining=0
        for name in "${names[@]}"; do
            if kubectl $KFN -n "$NAMESPACE" get stack "$name" &>/dev/null; then
                remaining=$((remaining + 1))
            fi
        done
        info "Stacks remaining to delete: $remaining/${#names[@]} (${elapsed}s/${timeout}s)"
        if [[ "$remaining" -eq 0 ]]; then
            return 0
        fi
        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_specific_stacks_ready() {
    local timeout="$1"
    shift
    local names=("$@")
    local start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        local ready=0
        for name in "${names[@]}"; do
            local is_ready=$(kubectl $KFN -n "$NAMESPACE" get stack "$name" -o json 2>/dev/null | \
                python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if any(c.get('type')=='Ready' and c.get('status')=='True' for c in d.get('status',{}).get('conditions',[])) else 'no')" 2>/dev/null || echo "no")
            if [[ "$is_ready" == "yes" ]]; then
                ready=$((ready + 1))
            fi
        done
        info "Stacks ready: $ready/${#names[@]} (${elapsed}s/${timeout}s)"
        if [[ "$ready" -eq "${#names[@]}" ]]; then
            return 0
        fi
        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

count_gcs_buckets() {
    local count
    count=$(GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets list \
        --project "$GCP_PROJECT" \
        --filter="labels.managed-by=pulumi-operator" \
        --format="value(name)" 2>/dev/null | wc -l | tr -d ' ')
    echo "${count:-0}"
}

# Find GCS bucket name by bucket-id label (e.g., "01")
find_bucket_by_id() {
    local bucket_id="$1"
    GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets list \
        --project "$GCP_PROJECT" \
        --filter="labels.bucket-id=${bucket_id} AND labels.managed-by=pulumi-operator" \
        --format="value(name)" 2>/dev/null
}

# Get a label value from a GCS bucket
get_bucket_label() {
    local bucket_name="$1" label_key="$2"
    GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets describe "gs://${bucket_name}" \
        --project "$GCP_PROJECT" --format="value(labels.${label_key})" 2>/dev/null || echo ""
}

# Verify a GCS bucket does NOT exist by bucket-id label
verify_bucket_deleted_by_id() {
    local bucket_id="$1"
    local found=$(find_bucket_by_id "$bucket_id")
    [[ -z "$found" ]]
}

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        log "  PASS: $desc (=$expected)"
        pass=$((pass + 1))
    else
        err "  FAIL: $desc -- expected='$expected', got='$actual'"
        fail=$((fail + 1))
    fi
}

assert_ge() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$actual" -ge "$expected" ]]; then
        log "  PASS: $desc ($actual >= $expected)"
        pass=$((pass + 1))
    else
        err "  FAIL: $desc -- expected >= $expected, got='$actual'"
        fail=$((fail + 1))
    fi
}

# ── Tests ─────────────────────────────────────────────────────────────────────

test_phase1_create_all() {
    log "================================================================"
    log "  PHASE 1: CREATE all 10 buckets concurrently"
    log "================================================================"

    for i in $(seq -w 1 $BUCKET_COUNT); do
        create_kustomization "bucket-${i}" "./infrastructure/bucket-${i}"
    done
    log "All $BUCKET_COUNT Kustomizations created"

    for i in $(seq -w 1 $BUCKET_COUNT); do
        kubectl $KFN -n flux-system wait kustomization/bucket-${i} --for=condition=Ready --timeout=120s 2>/dev/null || \
            warn "Kustomization bucket-${i} not Ready yet"
    done

    if wait_stacks_ready "$TIMEOUT" "$BUCKET_COUNT" "PHASE 1: CREATE"; then
        local ready_count=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d.get('items',[]) if any(c.get('type')=='Ready' and c.get('status')=='True' for c in i.get('status',{}).get('conditions',[]))))" 2>/dev/null || echo 0)
        assert_eq "All 10 stacks Ready" "$BUCKET_COUNT" "$ready_count"
    else
        err "CREATE phase timed out"
        dump_debug_info
        fail=$((fail + 1))
        return 1
    fi

    local bucket_count=$(count_gcs_buckets)
    assert_ge "GCS buckets exist on GCP" "$BUCKET_COUNT" "$bucket_count"

    # Verify each bucket independently on GCP by label
    log "Verifying each bucket independently on GCP..."
    for i in $(seq -w 1 $BUCKET_COUNT); do
        local bucket_name=$(find_bucket_by_id "$i")
        BUCKET_NAMES[$i]="$bucket_name"
        if [[ -n "$bucket_name" ]]; then
            local env_label=$(get_bucket_label "$bucket_name" "environment")
            assert_eq "Bucket ${i} ($bucket_name) exists with label environment=dev" "dev" "$env_label"
        else
            err "  FAIL: Bucket ${i} not found on GCP (no bucket with bucket-id=${i})"
            fail=$((fail + 1))
        fi
    done
}

test_phase2_mixed_update_and_delete() {
    log "================================================================"
    log "  PHASE 2: UPDATE buckets 01-05 AND DELETE buckets 06-10"
    log "  (concurrent mixed lifecycle operations)"
    log "================================================================"

    # UPDATE: patch buckets 01-05 to update overlay
    for i in 01 02 03 04 05; do
        kubectl $KFN -n flux-system patch kustomization bucket-${i} \
            --type merge -p "{\"spec\":{\"path\":\"./infrastructure/bucket-${i}-update\"}}"
    done
    log "Buckets 01-05: patched to update overlay"

    # Force reconcile updates
    for i in 01 02 03 04 05; do
        flux reconcile kustomization bucket-${i} -n flux-system --context="kind-$CLUSTER_NAME" 2>/dev/null || true
    done

    # DELETE: remove kustomizations 06-10 (triggers prune → destroy)
    for i in 06 07 08 09 10; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done
    log "Buckets 06-10: Kustomizations deleted (destroy via finalizer)"

    log "Waiting for mixed operations to complete..."
    local start_time=$(date +%s)
    local update_done=false
    local delete_done=false

    while true; do
        local elapsed=$(( $(date +%s) - start_time ))
        if [[ $elapsed -ge $TIMEOUT ]]; then
            err "Mixed phase timed out after ${TIMEOUT}s"
            dump_debug_info
            break
        fi

        show_mixed_progress "PHASE 2: MIXED UPDATE+DELETE" "$start_time"

        # Check updates (01-05 Ready)
        if [[ "$update_done" == "false" ]]; then
            local updated=0
            for i in 01 02 03 04 05; do
                local is_ready=$(kubectl $KFN -n "$NAMESPACE" get stack "gcs-bucket-${i}-dev" -o json 2>/dev/null | \
                    python3 -c "import json,sys; d=json.load(sys.stdin); lu=d.get('status',{}).get('lastUpdate',{}); print('yes' if lu.get('state')=='succeeded' and lu.get('generation',0)==d.get('metadata',{}).get('generation',0) else 'no')" 2>/dev/null || echo "no")
                if [[ "$is_ready" == "yes" ]]; then
                    updated=$((updated + 1))
                fi
            done
            if [[ "$updated" -ge 5 ]]; then
                log "  UPDATE complete: all 5 stacks (01-05) succeeded"
                update_done=true
            fi
        fi

        # Check deletes (06-10 gone)
        if [[ "$delete_done" == "false" ]]; then
            local deleted=0
            for i in 06 07 08 09 10; do
                if ! kubectl $KFN -n "$NAMESPACE" get stack "gcs-bucket-${i}-dev" &>/dev/null; then
                    deleted=$((deleted + 1))
                fi
            done
            if [[ "$deleted" -ge 5 ]]; then
                log "  DELETE complete: all 5 stacks (06-10) removed"
                delete_done=true
            fi
        fi

        if [[ "$update_done" == "true" && "$delete_done" == "true" ]]; then
            break
        fi

        sleep 10
    done

    # Assert results
    if [[ "$update_done" == "true" ]]; then
        log "  PASS: Buckets 01-05 updated successfully"
        pass=$((pass + 1))
    else
        err "  FAIL: Buckets 01-05 update did not complete"
        fail=$((fail + 1))
    fi

    if [[ "$delete_done" == "true" ]]; then
        log "  PASS: Buckets 06-10 destroyed successfully"
        pass=$((pass + 1))
    else
        err "  FAIL: Buckets 06-10 destroy did not complete"
        fail=$((fail + 1))
    fi

    # Verify updated buckets (01-05) have environment=staging label
    if [[ "$update_done" == "true" ]]; then
        log "Verifying updated bucket labels on GCP..."
        for i in 01 02 03 04 05; do
            local bucket_name=$(find_bucket_by_id "$i")
            if [[ -n "$bucket_name" ]]; then
                local env_label=$(get_bucket_label "$bucket_name" "environment")
                assert_eq "Bucket ${i} ($bucket_name) updated to environment=staging" "staging" "$env_label"
            else
                err "  FAIL: Updated bucket ${i} not found on GCP"
                fail=$((fail + 1))
            fi
        done
    fi

    # Verify destroyed buckets (06-10) no longer exist on GCP
    if [[ "$delete_done" == "true" ]]; then
        log "Verifying destroyed buckets removed from GCP..."
        for i in 06 07 08 09 10; do
            if verify_bucket_deleted_by_id "$i"; then
                log "  PASS: Bucket ${i} deleted from GCP"
                pass=$((pass + 1))
            else
                local leftover=$(find_bucket_by_id "$i")
                err "  FAIL: Bucket ${i} ($leftover) still exists on GCP"
                fail=$((fail + 1))
            fi
        done
    fi

    # Verify: exactly 5 GCS buckets remain (01-05)
    sleep 5
    local bucket_count=$(count_gcs_buckets)
    assert_eq "GCS buckets remaining (01-05 only)" "5" "$bucket_count"
}

test_phase3_delete_remaining() {
    log "================================================================"
    log "  PHASE 3: DELETE remaining buckets 01-05"
    log "================================================================"

    for i in 01 02 03 04 05; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done
    log "Buckets 01-05: Kustomizations deleted"

    local stack_names=(gcs-bucket-01-dev gcs-bucket-02-dev gcs-bucket-03-dev gcs-bucket-04-dev gcs-bucket-05-dev)
    if wait_specific_stacks_deleted "$TIMEOUT" "${stack_names[@]}"; then
        log "  PASS: All remaining stacks (01-05) deleted"
        pass=$((pass + 1))
    else
        err "  FAIL: Some stacks still exist"
        dump_debug_info
        fail=$((fail + 1))
    fi

    # Verify each bucket (01-05) individually deleted from GCP
    log "Verifying each bucket individually deleted from GCP..."
    sleep 10  # Allow GCS propagation
    for i in 01 02 03 04 05; do
        if verify_bucket_deleted_by_id "$i"; then
            log "  PASS: Bucket ${i} deleted from GCP"
            pass=$((pass + 1))
        else
            local leftover=$(find_bucket_by_id "$i")
            err "  FAIL: Bucket ${i} ($leftover) still exists on GCP"
            fail=$((fail + 1))
        fi
    done

    # Verify: zero GCS buckets remain
    local bucket_count=$(count_gcs_buckets)
    assert_eq "All GCS buckets cleaned up" "0" "$bucket_count"

    # Verify: no workspace pods
    local ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "No workspace pods remaining" "0" "$ws_count"

    # Verify: no stacks remain
    local stack_count=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "No stacks remaining" "0" "$stack_count"
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
    log "Starting mixed-lifecycle concurrent E2E tests..."
    log "  10 stacks, overlapping CREATE + UPDATE + DESTROY"
    log "  Test repo: $TEST_REPO_URL"
    log "  GCS state backend: gs://pulumi-state-pko-test"

    check_prereqs

    create_cluster
    build_and_load_image
    install_crds
    install_operator
    create_program_file_server_service
    install_flux
    setup_test_namespace
    create_flux_git_repository

    test_phase1_create_all
    test_phase2_mixed_update_and_delete
    test_phase3_delete_remaining

    echo ""
    log "========================================"
    log "Results: ${pass} passed, ${fail} failed"
    log "========================================"

    if [[ $fail -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
