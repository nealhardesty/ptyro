# ptyro — every workflow has a make target (see AGENTS.md). `make` alone lists them.

# rustup installs to ~/.cargo/bin; make it work even when that isn't on the caller's PATH yet.
CALLER_PATH := $(PATH)
export PATH := $(HOME)/.cargo/bin:$(PATH)
# Resolved with the PATH above: make execs simple recipes with its startup PATH, not the exported one.
CARGO ?= $(or $(shell PATH="$(PATH)" command -v cargo),cargo)
PTYRO := target/debug/ptyro
HEADLESS := target/debug/ptyro-headless
# Program for `make try` / `make headless` / `make fixture`.
CMD ?= $${SHELL:-sh}
# Extra flags: for `make run`, ptyro's arguments; for `make headless`/`fixture`, ptyro-headless's
# (e.g. ARGS='--size 100x30 --timeout 8 --snapshot 3').
ARGS ?=
# For `make fixture`: NAME and SIZE (COLSxROWS) of the recording.
NAME ?=
SIZE ?= 100x30
FIXTURES := crates/ptyro-term/tests/fixtures
# For `make parity`: STRICT=0 lists TUI gaps in docs/parity.md instead of failing on them;
# WEB_STRICT=0 lists web gaps instead of failing on them (ROADMAP WEB1 is done: they fail).
STRICT ?= 1
WEB_STRICT ?= 1

.DEFAULT_GOAL := help
.PHONY: help rust-install build release install test fmt fmt-check lint deny doc check ci clean tools \
        run try headless smoke fixture snapshots proptest-deep fuzz-smoke fuzz check-macos web web-test web-check ratchet ratchet-update lock-check msrv sbom installer-check dist-check bench criterion latency soak compat dist changelog homebrew-formula bump sandbox sandbox-clean scratchpad-demo orchestrate-demo changeset-demo verify-claude verify-structured tui-parity parity reset

help: ## List targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

## ── build ──────────────────────────────────────────────────────────────────
rust-install: ## Install rustup and the pinned Rust toolchain (Linux, macOS)
	PTYRO_CALLER_PATH='$(CALLER_PATH)' scripts/rust-install.sh

build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

release: ## Optimized build (target/release/ptyro)
	$(CARGO) build --workspace --release

install: ## Install the ptyro binary into ~/.cargo/bin
	$(CARGO) install --locked --path crates/ptyro

## ── quality ────────────────────────────────────────────────────────────────
test: ## Run all tests
	$(CARGO) test --workspace

fmt: ## Format all code
	$(CARGO) fmt --all

fmt-check: ## Fail if code is not formatted
	$(CARGO) fmt --all -- --check

lint: ## Clippy with warnings as errors
	$(CARGO) clippy --workspace --all-targets -- -D warnings

deny: ## License/advisory/source checks (needs `make tools`)
	$(CARGO) deny check

parity: ## Check every CLI command has a TUI path and a web path, or a waiver (docs/parity.md); a gap fails; STRICT=0 / WEB_STRICT=0 only list TUI / web gaps
	PTYRO_PARITY_STRICT=$(STRICT) PTYRO_PARITY_WEB_STRICT=$(WEB_STRICT) $(CARGO) test -p ptyro --bin ptyro parity -- --nocapture

tui-parity: parity ## The same as `make parity`

doc: ## Build API docs (a broken link or bad markup fails)
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps

lock-check: ## Fail if Cargo.lock is out of date with the Cargo.toml files
	$(CARGO) metadata --locked --format-version 1 >/dev/null

ratchet: ## Fail if a crate has more unwrap()/expect() in non-test code than scripts/unwrap-baseline.txt allows
	python3 scripts/unwrap-ratchet.py

ratchet-update: ## Write the current unwrap()/expect() counts as the new baseline (they should only go down)
	python3 scripts/unwrap-ratchet.py --update

# The oldest compiler the workspace claims to build with (`rust-version` in Cargo.toml).
MSRV := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1).0
msrv: ## Type-check the workspace with the oldest supported compiler (installs it with rustup)
	rustup toolchain install $(MSRV) --profile minimal
	$(CARGO) +$(MSRV) check --workspace --locked

check: fmt-check lint doc lock-check ratchet test smoke ## What CI's main job runs: fmt-check, lint, doc, lock-check, ratchet, test, smoke

ci: check deny web-check msrv fuzz-smoke installer-check ## Everything CI runs: check, deny, web-check, msrv, fuzz-smoke and installer-check

tools: ## Install developer tools (cargo-deny)
	$(CARGO) install --locked cargo-deny

clean: ## Remove build output
	$(CARGO) clean

## ── run & inspect ─────────────────────────────────────────────────────────
run: build ## Run ptyro with ARGS, e.g. make run ARGS="ls"
	$(PTYRO) $(ARGS)

try: build ## Start CMD in a new session and attach (the M1 demo), e.g. make try CMD=claude
	$(PTYRO) new -- $(CMD)

sandbox: build ## A throwaway ptyro (own daemon, store, worktrees: isolates its state, not a security sandbox) with a demo repo; ARGS go to ptyro
	scripts/sandbox.sh $(ARGS)

sandbox-clean: build ## Stop the sandbox's daemon and delete it
	scripts/sandbox.sh clean

reset: build ## Start ptyro over: stop it, remove its worktrees, service, config and data (summary, then asks; YES=1 skips asking unless work would be lost)
	YES=$(YES) scripts/reset.sh

scratchpad-demo: build ## NOTE1 in the sandbox: a pad with items, the CLI walkthrough (ARGS=tui to open the TUI on it, ARGS=clean)
	scripts/scratchpad-demo.sh $(ARGS)

orchestrate-demo: build ## M7 with fake agents: fan-out + compare + pick, a gated recipe, a lead over MCP (ARGS=tui|clean)
	scripts/orchestrate-demo.sh $(ARGS)

changeset-demo: build ## M11 with fake agents: one change set across three repos, contract to merge train (ARGS=tui|clean)
	scripts/changeset-demo.sh $(ARGS)

verify-claude: build ## Check the Claude adapter against the real Claude Code here (one tiny turn)
	scripts/verify-claude.sh

verify-structured: build ## Check structured mode against the real agents here (one tiny Claude turn; Codex/Gemini handshakes)
	scripts/verify-structured.sh

headless: build ## Run CMD in ptyro's emulator headless; prints screen + status report
	$(HEADLESS) $(ARGS) -- $(CMD)

smoke: build ## Quick end-to-end checks of PTY + emulator (used by CI)
	$(HEADLESS) --size 60x8 --timeout 5 --expect 'smoke ok' --expect 'wide:漢字' \
	  -- sh -c 'printf "\033[1;32msmoke ok\033[0m wide:漢字\n"'
	$(HEADLESS) --size 60x8 --timeout 5 --send '0.3:hello\r' --expect 'got=hello' \
	  -- sh -c 'read -r line; printf "got=%s\n" "$$line"'
	$(HEADLESS) --size 60x8 --timeout 5 --expect 'osc7501=yes' -- $(PTYRO) fake-agent

## ── test data ──────────────────────────────────────────────────────────────
fixture: build ## Record CMD's output to $(FIXTURES)/NAME.SIZE.ptylog (review + sanitize it!)
	@test -n "$(NAME)" || { echo "usage: make fixture NAME=vim-open SIZE=80x24 CMD='vim x' [ARGS=--send…]"; exit 2; }
	$(HEADLESS) --size $(SIZE) --record $(FIXTURES)/$(NAME).$(SIZE).ptylog $(ARGS) -- $(CMD)
	@echo "recorded $(FIXTURES)/$(NAME).$(SIZE).ptylog: review it for secrets (docs/fixtures.md), then: make snapshots"

snapshots: ## Accept golden snapshots: screens, CLI --json shapes, rc-file detection (review the git diff!)
	INSTA_UPDATE=always $(CARGO) test -p ptyro-term --test golden
	INSTA_UPDATE=always $(CARGO) test -p ptyro --test m5 json_shapes
	INSTA_UPDATE=always $(CARGO) test -p ptyro --test m7 json_shapes
	PTYRO_BLESS=1 $(CARGO) test -p ptyro-agents golden_rc_snippets
	@git status --short crates/ptyro-term/tests/snapshots crates/ptyro/tests/snapshots crates/ptyro-agents/tests/fixtures/rc

fuzz-smoke: ## Stable mutation fuzzing of every target for FUZZ_SECONDS (default 5) each (CI); crashes → target/fuzz-crashes
	$(CARGO) build --profile fuzz -p ptyro-testkit --bin ptyro-fuzz
	target/fuzz/ptyro-fuzz --seconds $(or $(FUZZ_SECONDS),5) $(if $(TARGET),--target $(TARGET))

fuzz: ## Coverage-guided fuzzing with cargo-fuzz (needs nightly + cargo-fuzz): make fuzz TARGET=emulator SECONDS=600
	@command -v cargo-fuzz >/dev/null || { echo "install it: rustup toolchain install nightly && cargo install cargo-fuzz"; exit 1; }
	cd fuzz && cargo +nightly fuzz run $(or $(TARGET),emulator) -- -max_total_time=$(or $(SECONDS),300)

compat: ## Headless compatibility sweep (vim, less, htop, tmux, zsh, python, vttest…) → Markdown table for docs/compat.md (VTTEST=path)
	$(CARGO) build -p ptyro-testkit --bin ptyro-headless
	scripts/compat.sh $(HEADLESS)

TSC ?= npx -y -p typescript@5.9.3 tsc

web: ## Build the web app (web/src → web/dist, embedded by `ptyro serve`; needs Node for tsc)
	cd web && $(TSC) -p tsconfig.app.json && $(TSC) -p tsconfig.sw.json
	python3 -I scripts/icons.py web/dist

web-test: ## The web app's logic tests that need no daemon (needs Node)
	node web/test/config.mjs units
	node web/test/term.mjs units
	node web/test/qr.mjs
	node web/test/kit.mjs
	node web/test/assets.mjs

web-check: ## Check that the committed web/dist matches web/src (CI)
	TSC="$(TSC)" scripts/web-check.sh

proptest-deep: ## Long property-test run (deltas == keyframes), 20k cases
	PROPTEST_CASES=20000 $(CARGO) test --release -p ptyro-term --test props

bench: release ## Emulator parse speed + daemon throughput/idle-CPU baseline (Linux; throwaway PTYRO_HOME)
	$(CARGO) test --release -p ptyro-term --test bench -- --ignored --nocapture 2>/dev/null | grep 'MB/s'
	scripts/bench.sh target/release/ptyro
	target/release/ptyro-latency --samples 300

criterion: ## Criterion micro-benchmarks: parse, frames, scrollback, handoff image, TUI paint (reports in target/criterion)
	$(CARGO) bench -p ptyro-term --bench term
	$(CARGO) bench -p ptyro-tui --bench paint

soak: release ## Soak: fake agents + shells + a live upgrade every UPGRADE_EVERY s, leak/stuck/lag checks (DURATION=10m; release: DURATION=7d)
	scripts/soak.sh target/release/ptyro

latency: release ## Key → echo → frame round trip through a real daemon, p50/p99 (ARGS='--samples 1000 --compress')
	target/release/ptyro-latency $(ARGS)

## ── release ────────────────────────────────────────────────────────────────
bump: ## Bump the version in Cargo.toml and Cargo.lock (V=patch|minor|major|X.Y.Z; default patch); then commit and tag
	CARGO=$(CARGO) scripts/bump.sh $(V)

dist: ## Release tarball + .sha256 for this platform in dist/ (TRIPLE=<rust target> to cross-build)
	TARGET=$(TRIPLE) CARGO=$(CARGO) scripts/dist.sh

changelog: ## Regenerate CHANGELOG.md from git history (tags → sections, commit prefixes → groups)
	scripts/changelog.sh > CHANGELOG.md

homebrew-formula: ## Render dist/ptyro.rb from the release checksums in dist/ (all four platforms)
	scripts/homebrew.sh

sbom: ## Software bill of materials (CycloneDX) from Cargo.lock: dist/ptyro-<version>.cdx.json
	python3 scripts/sbom.py

dist-check: ## Install the tarball in dist/ with scripts/install.sh into a temp prefix and run it (after `make dist`)
	TARGET=$(TRIPLE) scripts/installer-check.sh

installer-check: dist dist-check ## Build this platform's tarball and prove the installer script installs it

# Bundled C (SQLite, zstd, criterion's alloca) would need Apple's SDK, but `cargo check` never
# links, so a stand-in compiler (scripts/check-cc.sh) lets the whole workspace be checked.
check-macos: ## Type-check the whole workspace for macOS from Linux (CI builds everything on macOS)
	rustup target add aarch64-apple-darwin
	CC_aarch64_apple_darwin=$(CURDIR)/scripts/check-cc.sh AR_aarch64_apple_darwin=ar \
	  $(CARGO) check --workspace --all-targets --target aarch64-apple-darwin
