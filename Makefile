GUI_PKG := sim-gui
CORE_PKG := sim-core
# Same source of truth as the release workflow. Recursive, not `:=`, so only the
# bundle target pays for it.
VERSION = $(shell cargo metadata --format-version 1 --no-deps | \
	jq -r '.packages[] | select(.name == "$(GUI_PKG)") | .version')

.DEFAULT_GOAL := help
.PHONY: help build build-release run run-release check test test-core \
        clippy clippy-fix fmt fmt-check doc clean ci toolchain watch bundle-macos

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

# Word for word what .github/workflows/ci.yml runs. Without `-D warnings` this
# target printed its complaints and exited 0, so `make ci` went green on code
# the CI then refused: a gate that lets everything through is worse than none,
# because it is trusted.
clippy: ## Lint the whole workspace (build + tests), failing on any warning
	cargo clippy --workspace --all-targets -- -D warnings

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

third-party: ## Regenerate the notices of every crate we link against
	cargo about generate about.hbs -o THIRD-PARTY.md

third-party-check: ## Fail if THIRD-PARTY.md is behind the dependency tree
	@cargo about generate about.hbs -o /tmp/third-party-check.md
	@diff -q THIRD-PARTY.md /tmp/third-party-check.md >/dev/null \
		|| { echo "THIRD-PARTY.md is out of date, run: make third-party"; exit 1; }

ci: fmt-check clippy test ## Run all quality gates (format, lint, tests)

bundle-macos: ## Build the universal macOS .app locally (needs ~3 GB of disk)
	rustup target add aarch64-apple-darwin x86_64-apple-darwin
	cargo build --release -p $(GUI_PKG) --target aarch64-apple-darwin
	cargo build --release -p $(GUI_PKG) --target x86_64-apple-darwin
	packaging/macos/bundle.sh $(VERSION)

toolchain: ## Update the stable Rust toolchain via rustup
	rustup update stable

watch: ## Rebuild and rerun the GUI on every change (requires cargo-watch)
	cargo watch -x 'run -p $(GUI_PKG)'
