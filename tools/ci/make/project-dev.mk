# -------- Fuzzing --------

.PHONY: fuzz fuzz-build fuzz-list fuzz-run fuzz-clean fuzz-corpus

## Check cargo-fuzz is installed (required for fuzzing)
fuzz-install:
	$(call print_target_banner)
	$(call check_tool,cargo-fuzz)

## Build all fuzz targets
fuzz-build: fuzz-install
	$(call print_target_banner)
	cargo +nightly fuzz build --fuzz-dir tools/fuzz

## List all available fuzz targets
fuzz-list: fuzz-install
	$(call print_target_banner)
	cargo +nightly fuzz list --fuzz-dir tools/fuzz

## Run a specific fuzz target (use FUZZ_TARGET=name)
## Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=60
fuzz-run: fuzz-install
	$(call print_target_banner)
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz run --fuzz-dir tools/fuzz $(FUZZ_TARGET) -- -max_total_time=$(or $(FUZZ_SECONDS),60)

# Run all fuzz targets for a short time (smoke test)
fuzz: fuzz-build
	$(call print_target_banner)
	@echo "Running all fuzz targets for 30 seconds each..."
	@FAILED=0; \
	for target in $$(cargo +nightly fuzz list --fuzz-dir tools/fuzz); do \
		echo "=== Fuzzing $$target ==="; \
		cargo +nightly fuzz run --fuzz-dir tools/fuzz $$target -- -max_total_time=30 || FAILED=1; \
	done; \
	if [ $$FAILED -ne 0 ]; then \
		echo "Fuzzing found crashes. Check tools/fuzz/artifacts/ for details."; \
		exit 1; \
	fi
	@echo "Fuzzing complete. No crashes found."

## Clean fuzzing artifacts and corpus
fuzz-clean:
	$(call print_target_banner)
	rm -rf tools/fuzz/artifacts/
	rm -rf tools/fuzz/corpus/*/
	rm -rf tools/fuzz/target/

## Minimize corpus for a specific target
fuzz-corpus: fuzz-install
	$(call print_target_banner)
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-corpus FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz cmin --fuzz-dir tools/fuzz $(FUZZ_TARGET)

# -------- Mini chat --------

# mini-chat targets are for running the mini-chat gear locally and in Kubernetes, with options for building Docker images and deploying with Helm.

.PHONY: mini-chat mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward

# Run server with mini-chat gear
mini-chat:
	$(call print_target_banner)
	cargo run --bin $(EXAMPLE_SERVER_BIN) --features mini-chat,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/mini-chat.yaml run

## Build mini-chat Docker image for K8s (dev build by default, RELEASE=1 for optimized)
## On linux: builds on host (reuses local target/), then packages the binary.
## On other OS: full multi-stage Docker build with BuildKit caching.
MINI_CHAT_PROFILE = $(if $(RELEASE),release,dev)
MINI_CHAT_CARGO_RELEASE_FLAG = $(if $(RELEASE),--release,)
MINI_CHAT_TARGET_DIR = $(or $(CARGO_TARGET_DIR),target)/$(if $(RELEASE),release,debug)

mini-chat-docker:
	$(call print_target_banner)
ifeq ($(shell uname -s),Linux)
	@echo "==> Linux host: building on host, packaging into image"
	cargo build $(MINI_CHAT_CARGO_RELEASE_FLAG) --bin cf-gears-example-server --package=cf-gears-example-server \
		--features "$(MINI_CHAT_K8S_FEATURES)"
	@mkdir -p .docker-stage
	@cp $(MINI_CHAT_TARGET_DIR)/cf-gears-example-server .docker-stage/cf-gears-example-server
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat-prebuilt.Dockerfile \
		--build-arg BINARY_PATH=".docker-stage/cf-gears-example-server" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
	@rm -rf .docker-stage
else
	@echo "==> Non-linux host: full Docker build"
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat.Dockerfile \
		--build-arg CARGO_FEATURES="$(MINI_CHAT_K8S_FEATURES)" \
		--build-arg BUILD_PROFILE="$(MINI_CHAT_PROFILE)" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
endif

## Deploy mini-chat Helm chart to local K8s cluster (build + load + install)
mini-chat-helm: mini-chat-docker
	$(call print_target_banner)
	@if command -v k3s >/dev/null 2>&1; then \
		docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
	elif command -v minikube >/dev/null 2>&1; then \
		minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
		minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
	else \
		echo "ERROR: k3s or minikube required"; exit 1; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s

## Render mini-chat Helm templates (dry-run)
mini-chat-helm-template:
	$(call print_target_banner)
	helm template mini-chat gears/mini-chat/deploy/helm/mini-chat/

## One-command: ensure minikube is up, deploy latest chart, port-forward
## Usage: make mini-chat-up
## If image was rebuilt (make mini-chat-docker), re-run this to pick it up.
mini-chat-up:
	$(call print_target_banner)
	@# --- 1. Ensure cluster is running ---
	@if command -v minikube >/dev/null 2>&1; then \
		STATUS=$$(minikube status -f '{{.Host}}' 2>/dev/null || true); \
		if [ "$$STATUS" != "Running" ]; then \
			echo "Starting minikube..."; \
			minikube start; \
		fi; \
	elif command -v k3s >/dev/null 2>&1; then \
		: ; \
	else \
		echo "ERROR: minikube or k3s required"; exit 1; \
	fi
	@# --- 2. Load latest image if it exists locally ---
	@if docker image inspect $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) >/dev/null 2>&1; then \
		echo "Loading image $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) into cluster..."; \
		if command -v minikube >/dev/null 2>&1; then \
			minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
			minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
		else \
			docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
		fi; \
	else \
		echo "No local image found. Run 'make mini-chat-docker' first to build."; \
		exit 1; \
	fi
	@# --- 3. Helm install/upgrade ---
	@if [ -z "$${AZURE_OPENAI_API_KEY}" ] || [ -z "$${AZURE_OPENAI_API_HOST}" ]; then \
		echo "WARNING: AZURE_OPENAI_API_KEY or AZURE_OPENAI_API_HOST not set."; \
		echo "  export AZURE_OPENAI_API_KEY=... AZURE_OPENAI_API_HOST=..."; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s
	@echo ""
	@echo "mini-chat is running. In a separate terminal run:"
	@echo "  make mini-chat-port-forward"
	@echo "Then access: http://localhost:8087/cf/mini-chat"

## Persistent port-forward with auto-reconnect (run in a separate terminal)
mini-chat-port-forward:
	$(call print_target_banner)
	@echo "Port-forward: localhost:8087 -> svc/mini-chat:8087 (auto-reconnect, Ctrl+C to stop)"
	@while true; do \
		kubectl port-forward svc/mini-chat 8087:8087 2>&1 || true; \
		echo "connection lost, reconnecting in 2s..."; \
		sleep 2; \
	done

## Tear down mini-chat from the cluster
mini-chat-down:
	$(call print_target_banner)
	helm uninstall mini-chat 2>/dev/null || true
	@echo "mini-chat uninstalled"
