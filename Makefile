SHELL := /bin/bash
.DEFAULT_GOAL := help

# ——— Variables ———
VERSION    ?= $(shell cargo metadata --format-version 1 --no-deps | jq -r '.packages[0].version')
REGISTRY   ?= ghcr.io
IMAGE_NAME ?= $(REGISTRY)/pulumi/pulumi-rs-kube-operator
HELM_CHART ?= deploy/helm/pulumi-operator

# ——— Build ———

.PHONY: build
build: ## Build debug binary
	cargo build

.PHONY: build-release
build-release: ## Build optimised release binary
	cargo build --release

.PHONY: build-musl
build-musl: ## Build static musl binary (linux/amd64)
	cargo build --release --target x86_64-unknown-linux-musl

# ——— Test ———

.PHONY: test
test: ## Run all tests
	cargo test --all-targets

.PHONY: test-unit
test-unit: ## Run unit tests only
	cargo test --lib

.PHONY: test-property
test-property: ## Run property-based tests
	cargo test --test property

.PHONY: test-e2e
test-e2e: ## Run E2E tests (kind cluster, full lifecycle)
	./tests/e2e/run.sh

.PHONY: test-e2e-keep
test-e2e-keep: ## Run E2E tests, keep cluster for debugging
	KEEP_CLUSTER=true ./tests/e2e/run.sh

.PHONY: test-e2e-flux
test-e2e-flux: ## Run Flux CD E2E tests (kind + flux + full lifecycle)
	./tests/e2e/run-flux.sh

.PHONY: test-e2e-flux-keep
test-e2e-flux-keep: ## Run Flux CD E2E tests, keep cluster
	KEEP_CLUSTER=true ./tests/e2e/run-flux.sh

# ——— Lint ———

.PHONY: lint
lint: ## Run clippy + fmt check
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings

.PHONY: fmt
fmt: ## Auto-format code
	cargo fmt --all

# ——— Code Generation ———

.PHONY: generate-crds
generate-crds: ## Regenerate CRD YAML manifests
	cargo run --bin crdgen > /tmp/all-crds.yaml
	@awk 'BEGIN{n=0; file="/tmp/crd-"n".yaml"} /^---$$/{n++; file="/tmp/crd-"n".yaml"; next} {print > file}' /tmp/all-crds.yaml
	cp /tmp/crd-0.yaml deploy/crds/pulumi.com_stacks.yaml
	cp /tmp/crd-1.yaml deploy/crds/auto.pulumi.com_workspaces.yaml
	cp /tmp/crd-2.yaml deploy/crds/auto.pulumi.com_updates.yaml
	cp /tmp/crd-3.yaml deploy/crds/pulumi.com_programs.yaml
	cp deploy/crds/*.yaml $(HELM_CHART)/crds/
	@echo "CRDs generated and copied."

.PHONY: verify-crds
verify-crds: generate-crds ## Verify CRDs are up-to-date (fails if dirty)
	@git diff --exit-code deploy/crds/ $(HELM_CHART)/crds/ || \
		(echo "ERROR: CRDs are out of date. Run 'make generate-crds' and commit." && exit 1)

# ——— Docker ———

.PHONY: docker-build
docker-build: ## Build Docker image (multi-stage, current arch)
	docker build -t $(IMAGE_NAME):$(VERSION) .

.PHONY: docker-build-prebuilt
docker-build-prebuilt: build-musl ## Build Docker image from pre-built musl binary
	cp target/x86_64-unknown-linux-musl/release/pulumi-kubernetes-operator ./pulumi-kubernetes-operator
	docker build -f Dockerfile.release -t $(IMAGE_NAME):$(VERSION) .
	rm -f ./pulumi-kubernetes-operator

.PHONY: docker-push
docker-push: ## Push Docker image
	docker push $(IMAGE_NAME):$(VERSION)

# ——— Helm ———

.PHONY: helm-lint
helm-lint: ## Lint Helm chart
	helm lint $(HELM_CHART)

.PHONY: helm-template
helm-template: ## Render Helm templates locally
	helm template pulumi-operator $(HELM_CHART) --namespace pulumi-system

.PHONY: helm-package
helm-package: ## Package Helm chart
	helm package $(HELM_CHART)

# ——— Kubernetes ———

.PHONY: install-crds
install-crds: ## Install CRDs into current cluster
	kubectl apply -f deploy/crds/

.PHONY: deploy
deploy: install-crds ## Deploy operator via Helm to current cluster
	helm upgrade --install pulumi-operator $(HELM_CHART) \
		--namespace pulumi-system --create-namespace

.PHONY: undeploy
undeploy: ## Remove operator from current cluster
	helm uninstall pulumi-operator --namespace pulumi-system
	kubectl delete -f deploy/crds/ --ignore-not-found

# ——— Release ———

.PHONY: prep
prep: ## Prepare release (usage: make prep RELEASE=v2.1.0)
ifndef RELEASE
	$(error RELEASE is not set. Usage: make prep RELEASE=v2.1.0)
endif
	@V=$${RELEASE#v}; \
	sed -i.bak "s/^version = .*/version = \"$$V\"/" Cargo.toml && rm -f Cargo.toml.bak; \
	sed -i.bak "s/^version:.*/version: $$V/" $(HELM_CHART)/Chart.yaml && rm -f $(HELM_CHART)/Chart.yaml.bak; \
	sed -i.bak "s/^appVersion:.*/appVersion: \"$$V\"/" $(HELM_CHART)/Chart.yaml && rm -f $(HELM_CHART)/Chart.yaml.bak; \
	echo "Version updated to $$V"

# ——— Run ———

.PHONY: run
run: ## Run operator locally
	RUST_LOG=info cargo run -- operator

.PHONY: run-agent
run-agent: ## Run agent locally
	RUST_LOG=info cargo run -- agent

# ——— Help ———

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
