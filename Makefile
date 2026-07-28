GUI_PKG := sim-gui
CORE_PKG := sim-core

.DEFAULT_GOAL := help
.PHONY: help build build-release run run-release check test test-core \
        clippy clippy-fix fmt fmt-check doc clean ci toolchain watch

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-16s\033[0m %s\n", $$1, $$2}'

build: ## Build the workspace (dev profile)
	cargo build --workspace

build-release: ## Build the workspace (release profile)
	cargo build --workspace --release

run: ## Run the GUI (dev profile)
	cargo run -p $(GUI_PKG)

run-release: ## Run the GUI (release profile)
	cargo run -p $(GUI_PKG) --release

check: ## Type-check without producing binaries
	cargo check --workspace --all-targets

test: ## Run every test in the workspace
	cargo test --workspace

test-core: ## Run sim-core tests only (includes the UDP/TCP integration tests)
	cargo test -p $(CORE_PKG)

clippy: ## Lint the whole workspace (build + tests)
	cargo clippy --workspace --all-targets

clippy-fix: ## Apply automatic clippy fixes where possible
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

fmt: ## Format all workspace code
	cargo fmt --all

fmt-check: ## Check formatting without modifying files (used in CI)
	cargo fmt --all -- --check

doc: ## Build and open the workspace documentation
	cargo doc --workspace --no-deps --open

clean: ## Remove build artifacts
	cargo clean

ci: fmt-check clippy test ## Run all quality gates (format, lint, tests)

toolchain: ## Update the stable Rust toolchain via rustup
	rustup update stable

watch: ## Rebuild and rerun the GUI on every change (requires cargo-watch)
	cargo watch -x 'run -p $(GUI_PKG)'
