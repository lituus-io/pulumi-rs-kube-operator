# ============================================================================
# Multi-stage Dockerfile for pulumi-kubernetes-operator (Rust)
#
# Uses cargo-chef for dependency layer caching.
# Runtime image is debian-slim with Pulumi CLI installed.
#
# Build:
#   docker build -t pulumi-kubernetes-operator .
# ============================================================================

# ---------- Stage 1: Chef planner ----------
FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------- Stage 2: Build dependencies + binary ----------
FROM chef AS builder

ARG VERSION=dev

# Install protobuf compiler
RUN apt-get update -y && apt-get install -y \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Cook dependencies (cached layer — only rebuilt when Cargo.toml/lock change)
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json

# Build the actual binary
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release && \
    cp /app/target/release/pulumi-kubernetes-operator /app/operator

# ---------- Stage 3: Runtime with Pulumi CLI, git, tini ----------
FROM debian:trixie-20250224-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends --no-install-suggests \
    ca-certificates curl git tini \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# Install Pulumi CLI
ARG PULUMI_VERSION=3.216.0
ARG TARGETARCH
RUN PULUMI_ARCH=$(case "${TARGETARCH}" in arm64) echo "arm64";; *) echo "x64";; esac) && \
    curl -fsSL "https://get.pulumi.com/releases/sdk/pulumi-v${PULUMI_VERSION}-linux-${PULUMI_ARCH}.tar.gz" \
    | tar xz -C /usr/local/bin --strip-components=1

LABEL org.opencontainers.image.source="https://github.com/lituus-io/pulumi-rs-kube-operator"
LABEL org.opencontainers.image.description="Pulumi Kubernetes Operator (Rust)"
LABEL org.opencontainers.image.licenses="AGPL-3.0-or-later"
LABEL org.opencontainers.image.vendor="Lituus-io"

COPY --from=builder --chown=1000:1000 /app/operator /usr/local/bin/pulumi-kubernetes-operator
# Symlinks so bootstrap init container can find agent and tini at known paths
RUN ln -s /usr/local/bin/pulumi-kubernetes-operator /agent && \
    ln -s /usr/bin/tini /tini
RUN useradd -m -u 1000 pulumi

# Pre-install common Pulumi provider plugins as the pulumi user so they are
# available at runtime. Must run AFTER useradd so HOME resolves correctly.
ARG PULUMI_GCP_VERSION=8.0.0
USER 1000:1000
RUN pulumi plugin install resource gcp ${PULUMI_GCP_VERSION}

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -f http://localhost:8081/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/pulumi-kubernetes-operator"]
CMD ["operator"]
