# Pulumi Kubernetes Operator (Rust)

A Kubernetes operator for managing [Pulumi](https://www.pulumi.com/) stacks as native Kubernetes resources. Written in Rust for performance, safety, and minimal resource footprint.

## Features

- **Stack CRD** (`pulumi.com/v1`) — declarative Pulumi stack management with full lifecycle (create, update, preview, refresh, destroy)
- **Program CRD** (`pulumi.com/v1`) — inline Pulumi programs (YAML, Go, TypeScript, Python)
- **Workspace CRD** (`auto.pulumi.com/v1alpha1`) — ephemeral workspace pods for stack operations
- **Update CRD** (`auto.pulumi.com/v1alpha1`) — individual stack update tracking
- **GitOps Integration** — native Flux CD source support (GitRepository, OCIRepository, Bucket)
- **Webhook Support** — GitHub/GitLab webhook-triggered reconciliation with HMAC-SHA256 validation
- **Actor Model** — per-stack actors with configurable concurrency (default 25 concurrent reconciles)
- **Security Hardened** — zero `unsafe`, HMAC constant-time comparison, stderr secret redaction, gRPC size limits, webhook body limits

## Quick Start

### Prerequisites

- Kubernetes cluster (v1.28+)
- Helm v3
- Pulumi state backend (GCS, S3, Azure Blob, or file-based)

### Install via Helm

```bash
# Add CRDs
kubectl apply -f https://raw.githubusercontent.com/lituus-io/pulumi-rs-kube-operator/main/deploy/helm/pulumi-operator/crds/

# Method 1: OCI (recommended for Flux)
helm install pulumi-operator oci://ghcr.io/lituus-io/charts/pulumi-operator \
  --namespace pulumi-system --create-namespace

# Method 2: Traditional Helm repo
helm repo add pulumi-operator https://lituus-io.github.io/pulumi-rs-kube-operator
helm repo update
helm install pulumi-operator pulumi-operator/pulumi-operator \
  --namespace pulumi-system --create-namespace
```

### Install via Flux CD

```yaml
apiVersion: source.toolkit.fluxcd.io/v1
kind: HelmRepository
metadata:
  name: pulumi-operator
  namespace: flux-system
spec:
  type: oci
  interval: 10m
  url: oci://ghcr.io/lituus-io/charts
---
apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: pulumi-operator
  namespace: pulumi-system
spec:
  interval: 30m
  chart:
    spec:
      chart: pulumi-operator
      version: ">=0.1.0"
      sourceRef:
        kind: HelmRepository
        name: pulumi-operator
        namespace: flux-system
  install:
    createNamespace: true
```

### Example Stack

```yaml
apiVersion: pulumi.com/v1
kind: Stack
metadata:
  name: my-bucket
spec:
  stack: dev
  projectRepo: https://github.com/your-org/your-infra
  branch: refs/heads/main
  backend: gs://your-pulumi-state-bucket
  envRefs:
    PULUMI_CONFIG_PASSPHRASE:
      type: Secret
      secret:
        name: pulumi-secrets
        key: passphrase
    GOOGLE_CREDENTIALS:
      type: Secret
      secret:
        name: gcp-credentials
        key: credentials.json
  config:
    gcp:project: your-gcp-project
    gcp:region: us-central1
  destroyOnFinalize: true
```

## Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `operator.maxConcurrentReconciles` | `25` | Maximum concurrent stack reconciliations |
| `operator.leaderElect` | `true` | Enable leader election for HA |
| `operator.logLevel` | `info` | Log level (info, debug, trace) |
| `image.registry` | `ghcr.io` | Container image registry |
| `image.repository` | `lituus-io/pulumi-rs-kube-operator` | Container image repository |
| `networkPolicy.enabled` | `false` | Create a NetworkPolicy |

See [`deploy/helm/pulumi-operator/values.yaml`](deploy/helm/pulumi-operator/values.yaml) for all options.

## Development

```bash
# Build
cargo build --release

# Run tests
cargo test --all-targets

# Run clippy
cargo clippy --all-targets -- -D warnings

# Generate CRDs
cargo run --bin crdgen > /tmp/crds.yaml

# Run fuzz tests (requires nightly)
cargo +nightly fuzz run fuzz_stack_deser -- -max_total_time=60

# Helm lint
helm lint deploy/helm/pulumi-operator
```

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting and security practices.

## License

Copyright (c) 2025 Lituus-io. All rights reserved.

Dual-licensed under [AGPL-3.0-or-later](LICENSE-AGPL) and a commercial license.
See [LICENSE](LICENSE) for details.
