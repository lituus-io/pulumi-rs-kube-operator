#!/usr/bin/env bash
# =============================================================================
# E2E Test: Staggered concurrent lifecycle chaos with independent GCP verification.
#
# 10 stacks creating 10 GCS buckets, with CREATE / UPDATE / DESTROY operations
# fired in staggered, overlapping waves so that all three lifecycle phases are
# running simultaneously across different stacks.
#
# Wave schedule (wallclock after Phase 1 completes):
#   t=0   CREATE all 10 buckets ──────────────────────────────────────(Phase 1)
#   t=0   VERIFY: each bucket exists on GCP with environment=dev
#   t=0   VERIFY: all workspaces scale to zero
#
#   t=0   Wave A: UPDATE buckets 01,02,03       (dev → staging)       (Phase 2)
#   t=0   Wave B: DESTROY buckets 09,10         simultaneously
#   t=+5s Wave C: UPDATE bucket 04,05           (dev → staging)
#   t=+5s Wave D: DESTROY buckets 07,08         simultaneously
#   t=+10s Wave E: DESTROY bucket 06            while updates still settling
#         ──> At this point: 5 updating, 5 destroying, all concurrent
#
#   VERIFY each operation independently on GCP:
#     - 01-05: environment label changed to staging
#     - 06-10: bucket deleted from GCP
#     - All workspaces scale to zero
#
#   t=next Wave F: DESTROY updated buckets 01,02,03                  (Phase 3)
#   t=+3s  Wave G: DESTROY updated buckets 04,05
#         VERIFY: all 10 buckets gone, 0 stacks, 0 pods, 0 GCS buckets
#
# Metrics tracked:
#   - Per-phase wall-clock time
#   - Peak concurrent workspace pod count
#   - Individual bucket GCP assertions (create/update/destroy)
#   - Total assertions: 50+
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CLUSTER_NAME="${CLUSTER_NAME:-pko-flux-chaos}"
NAMESPACE="${NAMESPACE:-pulumi-test}"
IMAGE_NAME="${IMAGE_NAME:-pulumi-rs-kube-operator:e2e}"
KEEP_CLUSTER="${KEEP_CLUSTER:-false}"
TIMEOUT="${TIMEOUT:-900}"
BUCKET_COUNT=10
CREDS_FILE="${CREDS_FILE:-/Users/gatema/Desktop/drive/git/code/creds/terraform.json}"
TEST_REPO_URL="${TEST_REPO_URL:-https://github.com/terekete/pulumi-rs-kube-operator-test}"
GCLOUD="${GCLOUD:-$HOME/google-cloud-sdk/bin/gcloud}"
GCP_PROJECT="${GCP_PROJECT:-spacy-muffin-lab-5a292e}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
BOLD='\033[1m'
NC='\033[0m'

pass=0
fail=0
peak_pods=0
declare -A PHASE_TIMES

log()  { echo -e "${GREEN}[CHAOS-E2E]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[FAIL]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC} $*"; }
perf() { echo -e "${MAGENTA}[PERF]${NC} $*"; }

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
    warn "=== Operator logs (last 100) ==="
    kubectl $KFN -n pulumi-system logs -l app.kubernetes.io/name=pulumi-operator --tail=100 2>/dev/null | grep -v '"level":"DEBUG"' || true
    warn "=== Stacks ==="
    kubectl $KFN -n "$NAMESPACE" get stacks -o wide 2>/dev/null || true
    warn "=== Updates ==="
    kubectl $KFN -n "$NAMESPACE" get updates -o wide 2>/dev/null || true
    warn "=== Pods ==="
    kubectl $KFN -n "$NAMESPACE" get pods -o wide 2>/dev/null || true
}

# ── Dashboard ─────────────────────────────────────────────────────────────────

track_peak_pods() {
    local pods
    pods=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
    if [[ "$pods" -gt "$peak_pods" ]]; then
        peak_pods=$pods
    fi
}

show_chaos_dashboard() {
    local phase="$1"
    local start_time="$2"
    local elapsed=$(( $(date +%s) - start_time ))

    echo -e "\n${BOLD}══════ ${phase} (${elapsed}s) ══════${NC}"

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
    gen = item.get('metadata', {}).get('generation', 0)
    obs_gen = last.get('generation', 0)
    deleting = item['metadata'].get('deletionTimestamp') is not None
    stale = '(stale)' if gen != obs_gen and not deleting else ''
    if deleting:
        status = '\033[0;31mDELETING\033[0m'
    elif ready:
        status = '\033[0;32mREADY\033[0m'
    elif reconciling:
        status = '\033[0;36mRECONCILING\033[0m'
    else:
        status = '\033[1;33mWAITING\033[0m'
    print(f'  {name:30s} {status:22s} {utype}/{state} {stale}')
" 2>/dev/null || echo "  (no stacks)"

    local pods
    pods=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
    echo -e "  Workspace pods: ${CYAN}${pods}${NC}  (peak: ${peak_pods})"
    track_peak_pods
    echo ""
}

# ── Prerequisites ─────────────────────────────────────────────────────────────

check_prereqs() {
    local missing=()
    for cmd in kind kubectl helm docker flux python3; do
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
    if ! "$GCLOUD" --version &>/dev/null; then
        err "gcloud not found at $GCLOUD"
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

# ── GCP Verification Helpers ─────────────────────────────────────────────────

find_bucket_by_id() {
    local bucket_id="$1"
    GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets list \
        --project "$GCP_PROJECT" \
        --filter="labels.bucket-id=${bucket_id} AND labels.managed-by=pulumi-operator" \
        --format="value(name)" 2>/dev/null
}

get_bucket_label() {
    local bucket_name="$1" label_key="$2"
    GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets describe "gs://${bucket_name}" \
        --project "$GCP_PROJECT" --format="value(labels.${label_key})" 2>/dev/null || echo ""
}

count_gcs_buckets() {
    local count
    count=$(GOOGLE_APPLICATION_CREDENTIALS="$CREDS_FILE" "$GCLOUD" storage buckets list \
        --project "$GCP_PROJECT" \
        --filter="labels.managed-by=pulumi-operator" \
        --format="value(name)" 2>/dev/null | wc -l | tr -d ' ')
    echo "${count:-0}"
}

verify_bucket_exists_with_label() {
    local bucket_id="$1" label_key="$2" expected_value="$3"
    local bucket_name
    bucket_name=$(find_bucket_by_id "$bucket_id")
    if [[ -z "$bucket_name" ]]; then
        return 1
    fi
    local actual_value
    actual_value=$(get_bucket_label "$bucket_name" "$label_key")
    [[ "$actual_value" == "$expected_value" ]]
}

verify_bucket_deleted_by_id() {
    local bucket_id="$1"
    local found
    found=$(find_bucket_by_id "$bucket_id")
    [[ -z "$found" ]]
}

# ── Operator State Helpers ────────────────────────────────────────────────────

wait_stacks_ready() {
    local timeout="$1" target="$2" phase="$3"
    local start_time
    start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        show_chaos_dashboard "$phase" "$start_time"
        local ready_count
        ready_count=$(kubectl $KFN -n "$NAMESPACE" get stacks -o json 2>/dev/null | \
            python3 -c "
import json,sys
d = json.load(sys.stdin)
print(sum(1 for i in d.get('items',[])
    if any(c.get('type')=='Ready' and c.get('status')=='True'
           for c in i.get('status',{}).get('conditions',[]))))
" 2>/dev/null || echo 0)
        if [[ "$ready_count" -ge "$target" ]]; then
            return 0
        fi
        sleep 10
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

# Wait for specific stacks to have lastUpdate at current generation with state=succeeded
wait_stacks_updated() {
    local timeout="$1"
    shift
    local names=("$@")
    local start_time
    start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        local done_count=0
        for name in "${names[@]}"; do
            local ok
            ok=$(kubectl $KFN -n "$NAMESPACE" get stack "$name" -o json 2>/dev/null | \
                python3 -c "
import json,sys
d = json.load(sys.stdin)
lu = d.get('status',{}).get('lastUpdate',{})
gen = d.get('metadata',{}).get('generation',0)
print('yes' if lu.get('state')=='succeeded' and lu.get('generation',0)==gen else 'no')
" 2>/dev/null || echo "no")
            if [[ "$ok" == "yes" ]]; then
                done_count=$((done_count + 1))
            fi
        done
        if [[ "$done_count" -ge "${#names[@]}" ]]; then
            return 0
        fi
        sleep 8
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_stacks_deleted() {
    local timeout="$1"
    shift
    local names=("$@")
    local start_time
    start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        local remaining=0
        for name in "${names[@]}"; do
            if kubectl $KFN -n "$NAMESPACE" get stack "$name" &>/dev/null; then
                remaining=$((remaining + 1))
            fi
        done
        info "Delete wait: ${remaining}/${#names[@]} remaining (${elapsed}s)"
        if [[ "$remaining" -eq 0 ]]; then
            return 0
        fi
        sleep 8
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

wait_workspaces_zero() {
    local timeout="$1"
    local start_time
    start_time=$(date +%s)
    local elapsed=0
    while [[ $elapsed -lt $timeout ]]; do
        local ws_count
        ws_count=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
        if [[ "$ws_count" -eq 0 ]]; then
            return 0
        fi
        info "Workspace pods: $ws_count (${elapsed}s)"
        sleep 5
        elapsed=$(( $(date +%s) - start_time ))
    done
    return 1
}

# ── Assertion Helpers ─────────────────────────────────────────────────────────

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

assert_true() {
    local desc="$1" result="$2"
    if [[ "$result" == "true" ]]; then
        log "  PASS: $desc"
        pass=$((pass + 1))
    else
        err "  FAIL: $desc"
        fail=$((fail + 1))
    fi
}

# ── Phase 1: Create All 10 Buckets ──────────────────────────────────────────

test_phase1_create_all() {
    log "╔══════════════════════════════════════════════════════════════╗"
    log "║  PHASE 1: CREATE all 10 buckets concurrently               ║"
    log "╚══════════════════════════════════════════════════════════════╝"
    local phase_start
    phase_start=$(date +%s)

    # Fire all 10 kustomizations at once
    for i in $(seq -w 1 $BUCKET_COUNT); do
        create_kustomization "bucket-${i}" "./infrastructure/bucket-${i}"
    done
    log "All $BUCKET_COUNT Kustomizations submitted"

    # Wait for Flux to reconcile them
    for i in $(seq -w 1 $BUCKET_COUNT); do
        kubectl $KFN -n flux-system wait kustomization/bucket-${i} --for=condition=Ready --timeout=120s 2>/dev/null || \
            warn "Kustomization bucket-${i} slow"
    done

    # Wait for all stacks to become Ready
    if ! wait_stacks_ready "$TIMEOUT" "$BUCKET_COUNT" "PHASE 1: CREATE"; then
        err "CREATE phase timed out after ${TIMEOUT}s"
        dump_debug_info
        fail=$((fail + 1))
        return 1
    fi

    local phase_elapsed=$(( $(date +%s) - phase_start ))
    PHASE_TIMES[create]=$phase_elapsed
    perf "Phase 1 CREATE completed in ${phase_elapsed}s"

    # ── Independent GCP verification: each bucket exists with environment=dev ──
    log "Verifying each bucket independently on GCP..."
    for i in $(seq -w 1 $BUCKET_COUNT); do
        local bucket_name
        bucket_name=$(find_bucket_by_id "$i")
        if [[ -n "$bucket_name" ]]; then
            local env_label
            env_label=$(get_bucket_label "$bucket_name" "environment")
            assert_eq "Bucket $i exists with environment=dev" "dev" "$env_label"
            local bid_label
            bid_label=$(get_bucket_label "$bucket_name" "bucket-id")
            assert_eq "Bucket $i has correct bucket-id label" "$i" "$bid_label"
        else
            err "  FAIL: Bucket $i not found on GCP"
            fail=$((fail + 2))
        fi
    done

    local total_buckets
    total_buckets=$(count_gcs_buckets)
    assert_eq "Total GCS buckets on GCP" "$BUCKET_COUNT" "$total_buckets"

    # ── Verify workspaces scale to zero ──
    log "Checking workspace scale-to-zero after CREATE..."
    if wait_workspaces_zero 120; then
        assert_true "Workspaces scaled to zero after CREATE" "true"
    else
        local ws_remaining
        ws_remaining=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
        err "  FAIL: $ws_remaining workspace pods still running after CREATE"
        fail=$((fail + 1))
    fi
}

# ── Phase 2: Staggered UPDATE + DESTROY Chaos ───────────────────────────────

test_phase2_staggered_chaos() {
    log "╔══════════════════════════════════════════════════════════════╗"
    log "║  PHASE 2: STAGGERED CHAOS                                  ║"
    log "║    Wave A: UPDATE 01,02,03                                 ║"
    log "║    Wave B: DESTROY 09,10             (simultaneous)        ║"
    log "║    Wave C: UPDATE 04,05              (+5s)                 ║"
    log "║    Wave D: DESTROY 07,08             (+5s)                 ║"
    log "║    Wave E: DESTROY 06                (+10s)                ║"
    log "╚══════════════════════════════════════════════════════════════╝"
    local phase_start
    phase_start=$(date +%s)

    # ── Wave A: UPDATE buckets 01,02,03 ──
    log "Wave A: UPDATE buckets 01,02,03 (dev → staging)"
    for i in 01 02 03; do
        kubectl $KFN -n flux-system patch kustomization bucket-${i} \
            --type merge -p "{\"spec\":{\"path\":\"./infrastructure/bucket-${i}-update\"}}"
    done
    for i in 01 02 03; do
        flux reconcile kustomization bucket-${i} -n flux-system --context="kind-$CLUSTER_NAME" 2>/dev/null || true
    done

    # ── Wave B: DESTROY buckets 09,10 (simultaneously with Wave A) ──
    log "Wave B: DESTROY buckets 09,10"
    for i in 09 10; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done

    # ── Wave C: UPDATE buckets 04,05 (+5s stagger) ──
    sleep 5
    log "Wave C: UPDATE buckets 04,05 (dev → staging)"
    for i in 04 05; do
        kubectl $KFN -n flux-system patch kustomization bucket-${i} \
            --type merge -p "{\"spec\":{\"path\":\"./infrastructure/bucket-${i}-update\"}}"
    done
    for i in 04 05; do
        flux reconcile kustomization bucket-${i} -n flux-system --context="kind-$CLUSTER_NAME" 2>/dev/null || true
    done

    # ── Wave D: DESTROY buckets 07,08 (+5s stagger from Wave C) ──
    log "Wave D: DESTROY buckets 07,08"
    for i in 07 08; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done

    # ── Wave E: DESTROY bucket 06 (+5s stagger) ──
    sleep 5
    log "Wave E: DESTROY bucket 06"
    kubectl $KFN -n flux-system delete kustomization bucket-06 --wait=false 2>/dev/null || true

    log "All waves fired. Waiting for concurrent operations to settle..."

    # ── Wait for updates (01-05) ──
    local update_names=(gcs-bucket-01-dev gcs-bucket-02-dev gcs-bucket-03-dev gcs-bucket-04-dev gcs-bucket-05-dev)
    local update_done=false
    if wait_stacks_updated "$TIMEOUT" "${update_names[@]}"; then
        update_done=true
        log "  UPDATE complete: stacks 01-05 succeeded at current generation"
    else
        warn "  UPDATE may not have completed for all stacks 01-05"
    fi

    # ── Wait for deletes (06-10) ──
    local delete_names=(gcs-bucket-06-dev gcs-bucket-07-dev gcs-bucket-08-dev gcs-bucket-09-dev gcs-bucket-10-dev)
    local delete_done=false
    if wait_stacks_deleted "$TIMEOUT" "${delete_names[@]}"; then
        delete_done=true
        log "  DELETE complete: stacks 06-10 removed"
    else
        warn "  DELETE may not have completed for all stacks 06-10"
    fi

    local phase_elapsed=$(( $(date +%s) - phase_start ))
    PHASE_TIMES[chaos]=$phase_elapsed
    perf "Phase 2 CHAOS completed in ${phase_elapsed}s"

    # ── Verify updates (01-05): environment label changed to staging on GCP ──
    log "Verifying UPDATE results on GCP..."
    for i in 01 02 03 04 05; do
        local bucket_name
        bucket_name=$(find_bucket_by_id "$i")
        if [[ -n "$bucket_name" ]]; then
            local env_label
            env_label=$(get_bucket_label "$bucket_name" "environment")
            assert_eq "Bucket $i updated to environment=staging" "staging" "$env_label"
        else
            err "  FAIL: Updated bucket $i not found on GCP"
            fail=$((fail + 1))
        fi
    done

    if [[ "$update_done" == "true" ]]; then
        assert_true "All 5 UPDATE operations succeeded" "true"
    else
        err "  FAIL: Not all UPDATE operations completed"
        fail=$((fail + 1))
        dump_debug_info
    fi

    # ── Verify deletes (06-10): buckets removed from GCP ──
    log "Verifying DESTROY results on GCP..."
    sleep 5  # Allow GCS propagation
    for i in 06 07 08 09 10; do
        if verify_bucket_deleted_by_id "$i"; then
            assert_true "Bucket $i deleted from GCP" "true"
        else
            local leftover
            leftover=$(find_bucket_by_id "$i")
            err "  FAIL: Bucket $i ($leftover) still exists on GCP"
            fail=$((fail + 1))
        fi
    done

    if [[ "$delete_done" == "true" ]]; then
        assert_true "All 5 DESTROY operations succeeded" "true"
    else
        err "  FAIL: Not all DESTROY operations completed"
        fail=$((fail + 1))
        dump_debug_info
    fi

    # ── Verify bucket count: exactly 5 remaining ──
    local remaining_buckets
    remaining_buckets=$(count_gcs_buckets)
    assert_eq "GCS buckets remaining after chaos (01-05 only)" "5" "$remaining_buckets"

    # ── Verify workspaces scale to zero ──
    log "Checking workspace scale-to-zero after CHAOS..."
    if wait_workspaces_zero 120; then
        assert_true "Workspaces scaled to zero after chaos" "true"
    else
        local ws_remaining
        ws_remaining=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
        err "  FAIL: $ws_remaining workspace pods still running after chaos"
        fail=$((fail + 1))
    fi
}

# ── Phase 3: Staggered Cleanup of Remaining Buckets ─────────────────────────

test_phase3_staggered_destroy() {
    log "╔══════════════════════════════════════════════════════════════╗"
    log "║  PHASE 3: STAGGERED DESTROY remaining 01-05               ║"
    log "║    Wave F: DESTROY 01,02,03                                ║"
    log "║    Wave G: DESTROY 04,05              (+3s)                ║"
    log "╚══════════════════════════════════════════════════════════════╝"
    local phase_start
    phase_start=$(date +%s)

    # Wave F: destroy 01,02,03
    log "Wave F: DESTROY buckets 01,02,03"
    for i in 01 02 03; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done

    sleep 3

    # Wave G: destroy 04,05
    log "Wave G: DESTROY buckets 04,05"
    for i in 04 05; do
        kubectl $KFN -n flux-system delete kustomization bucket-${i} --wait=false 2>/dev/null || true
    done

    local all_names=(gcs-bucket-01-dev gcs-bucket-02-dev gcs-bucket-03-dev gcs-bucket-04-dev gcs-bucket-05-dev)
    if wait_stacks_deleted "$TIMEOUT" "${all_names[@]}"; then
        assert_true "All stacks 01-05 deleted" "true"
    else
        err "  FAIL: Some stacks 01-05 still exist"
        dump_debug_info
        fail=$((fail + 1))
    fi

    local phase_elapsed=$(( $(date +%s) - phase_start ))
    PHASE_TIMES[destroy_remaining]=$phase_elapsed
    perf "Phase 3 DESTROY completed in ${phase_elapsed}s"

    # ── Verify each bucket individually deleted from GCP ──
    log "Verifying each remaining bucket deleted from GCP..."
    sleep 10  # GCS propagation
    for i in 01 02 03 04 05; do
        if verify_bucket_deleted_by_id "$i"; then
            assert_true "Bucket $i deleted from GCP" "true"
        else
            local leftover
            leftover=$(find_bucket_by_id "$i")
            err "  FAIL: Bucket $i ($leftover) still exists on GCP"
            fail=$((fail + 1))
        fi
    done

    # ── Final state assertions ──
    local final_buckets
    final_buckets=$(count_gcs_buckets)
    assert_eq "All GCS buckets cleaned up" "0" "$final_buckets"

    local final_stacks
    final_stacks=$(kubectl $KFN -n "$NAMESPACE" get stacks --no-headers 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "No stacks remaining" "0" "$final_stacks"

    # ── Final workspace scale-to-zero ──
    log "Final workspace scale-to-zero check..."
    if wait_workspaces_zero 60; then
        assert_true "Zero workspace pods at end" "true"
    else
        local ws_final
        ws_final=$(kubectl $KFN -n "$NAMESPACE" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
        err "  FAIL: $ws_final workspace pods remain"
        fail=$((fail + 1))
    fi

    local final_updates
    final_updates=$(kubectl $KFN -n "$NAMESPACE" get updates --no-headers 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "No updates remaining (garbage collected)" "0" "$final_updates"
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
    log "╔══════════════════════════════════════════════════════════════╗"
    log "║  Staggered Chaos E2E: 10 stacks, overlapping lifecycles    ║"
    log "║  Concurrent CREATE + UPDATE + DESTROY across stacks        ║"
    log "╚══════════════════════════════════════════════════════════════╝"
    log ""
    log "  Test repo:      $TEST_REPO_URL"
    log "  GCS backend:    gs://pulumi-state-pko-test"
    log "  GCP project:    $GCP_PROJECT"
    log "  Timeout:        ${TIMEOUT}s"
    log ""

    local total_start
    total_start=$(date +%s)

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
    test_phase2_staggered_chaos
    test_phase3_staggered_destroy

    local total_elapsed=$(( $(date +%s) - total_start ))

    echo ""
    log "╔══════════════════════════════════════════════════════════════╗"
    log "║  RESULTS                                                    ║"
    log "╠══════════════════════════════════════════════════════════════╣"
    log "║  Assertions:  ${pass} passed, ${fail} failed"
    log "╠══════════════════════════════════════════════════════════════╣"
    log "║  Performance:                                               ║"
    perf "  Phase 1 CREATE (10 buckets):     ${PHASE_TIMES[create]:-?}s"
    perf "  Phase 2 CHAOS (5 update+5 del):  ${PHASE_TIMES[chaos]:-?}s"
    perf "  Phase 3 DESTROY (5 remaining):   ${PHASE_TIMES[destroy_remaining]:-?}s"
    perf "  Total wall-clock:                ${total_elapsed}s"
    perf "  Peak concurrent workspace pods:  ${peak_pods}"
    log "╚══════════════════════════════════════════════════════════════╝"

    if [[ $fail -gt 0 ]]; then
        err "TEST SUITE FAILED"
        exit 1
    else
        log "ALL TESTS PASSED"
    fi
}

main "$@"
