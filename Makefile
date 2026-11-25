# ptyro — every workflow has a make target (see AGENTS.md). `make` alone lists them.

# rustup installs to ~/.cargo/bin; make it work even when that isn't on the caller's PATH yet.
CARGO_BIN := $(HOME)/.cargo/bin
CALLER_PATH := $(PATH)
export PATH := $(CARGO_BIN):$(PATH)
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
.PHONY: help rust-install build release install install-release daemon-upgrade test fmt fmt-check lint deny doc check ci clean tools \
        run try headless smoke fixture snapshots proptest-deep fuzz-smoke fuzz check-macos web web-test web-e2e web-check ratchet ratchet-update catalog-fixtures lock-check msrv sbom installer-check dist-check brew-check bench criterion latency soak compat dist changelog homebrew-formula bump sandbox sandbox-clean verify-claude verify-structured verify-kb verify-shells spike-chat spike-chat-fts spike-kb spike-kb-comments bench-kb-redact bench-kb-offer kb-eval kb-eval-real spike-wsl test-wsl tui-parity parity reset

help: ## List targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

## ── build ──────────────────────────────────────────────────────────────────
rust-install: ## Install rustup and the pinned Rust toolchain (Linux, macOS; on Windows, inside WSL2)
	PTYRO_CALLER_PATH='$(CALLER_PATH)' sh scripts/rust-install.sh

build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

release: ## Optimized build (target/release/ptyro)
	$(CARGO) build --workspace --release

# `cargo install` builds in a throwaway directory, so every run recompiled every dependency
# (minutes). This builds in target/ (kept between runs, incremental) and copies the binary over
# the old one by rename, so a running daemon keeps its old copy until it restarts.
install: ## Install the ptyro binary into ~/.cargo/bin (a fast build: seconds after the first)
	$(CARGO) build --locked --profile local -p ptyro
	mkdir -p $(HOME)/.cargo/bin
	install -m 755 target/local/ptyro $(HOME)/.cargo/bin/ptyro.new
	mv -f $(HOME)/.cargo/bin/ptyro.new $(HOME)/.cargo/bin/ptyro

daemon-upgrade: install ## Install, then move the running daemon onto the new binary (a live upgrade: sessions keep running)
	$(HOME)/.cargo/bin/ptyro daemon upgrade

install-release: ## Install the fully optimized build (what a release ships: slow, thin LTO)
	$(CARGO) install --locked --path crates/ptyro

## ── quality ────────────────────────────────────────────────────────────────
# `make test` runs every test binary through ptyro-flaky (WIN13): a test that fails runs again
# once, alone; passing then, it is a flake, named in the output, target/flakes.txt and CI's job
# summary. More than FLAKE_BUDGET flakes, or a test failing twice, fails the run.
FLAKE_BUDGET ?= 2
# macOS runners have three cores: m7's nine tests at once took 102 s there, and missed windows.
TEST_ENV = $(if $(filter Darwin,$(shell uname -s)),RUST_TEST_THREADS=2)

test: ## Run all tests (every test binary runs; a failing test runs again once, and more than FLAKE_BUDGET=2 flakes fail the run)
	PTYRO_WSL_HOLD=0 FLAKE_BUDGET=$(FLAKE_BUDGET) $(TEST_ENV) $(CARGO) run -q -p ptyro-testkit --bin ptyro-flaky -- --workspace

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

catalog-fixtures: ## Re-record the version table's newest-version replies (npm, GitHub, Claude's feed, nodejs.org) as test fixtures, printing each newest version (needs the network; review the diff)
	python3 scripts/catalog-fixtures.py

# The oldest compiler the workspace claims to build with (`rust-version` in Cargo.toml).
MSRV := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1).0
msrv: ## Type-check the workspace with the oldest supported compiler (installs it with rustup)
	rustup toolchain install $(MSRV) --profile minimal
	$(CARGO) +$(MSRV) check --workspace --locked

check: fmt-check lint doc lock-check ratchet test smoke ## What CI's main job runs: fmt-check, lint, doc, lock-check, ratchet, test, smoke

ci: check deny web-check web-e2e msrv fuzz-smoke installer-check ## Everything CI runs: check, deny, web-check, web-e2e, msrv, fuzz-smoke and installer-check

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

verify-claude: build ## Check the Claude adapter against the real Claude Code here (one tiny turn)
	scripts/verify-claude.sh

verify-structured: build ## Check structured mode, what chats run on, against the real agents here (one tiny chat each)
	scripts/verify-structured.sh

verify-kb: ## KB0, KB1.4, KB1.10, KB5: check what the knowledge base rests on against the real Claude Code and Codex here: hooks, transcripts and ptyro's own reader over them, MCP tools, and the tools as built (ARGS="claude" for one, "--built" for only the as-built tools, "--offer" for only KB5.1's check of a prompt hook's added context, "--offer-built" for only what ptyro itself offers at a prompt, "--fixtures" to save sanitized transcripts; a few cents of tokens)
	python3 -B scripts/verify-kb.py $(ARGS)

spike-chat-fts: ## CHAT0: FTS5 and the porter tokenizer in the bundled SQLite, and an index over NODES (default 100000) chat nodes
	NODES=$${NODES:-100000} $(CARGO) run --profile local -p ptyro-store --example chat_fts_spike

spike-chat-meaning: ## CHAT5: meaning search's cost over NODES (default 100000) chat messages: embedding, the backfill, a search's scan, the vectors' size (synthetic model, no download)
	NODES=$${NODES:-100000} $(CARGO) run --profile local -p ptyro-store --example chat_meaning_spike

spike-kb: ## KB0.1, KB1.5: the knowledge base's search at size, over CHUNKS (default 500000; also run 1000000) synthetic chunks in SPEC §25.4's schema: by words, by meaning with each filter, the fallbacks and their recall, the merged search, the same through the search as built (KbDb, the fusion, the ranking), the file's size, ingest and forget (synthetic model, no download; about 2 minutes and 600 MB of temporary disk)
	CHUNKS=$${CHUNKS:-500000} $(CARGO) run --profile local -p ptyro-store --example kb_spike

spike-kb-comments: ## KB0.4: run the comment table (SPEC §25.6.3) over real code, read-only: ARGS="<folder>…" (default: this repository); counts what is kept and what each rule drops, with SAMPLE (default 8) of each to judge by eye
	$(CARGO) run -q -p ptyro-core --example kb_comments -- $(or $(ARGS),.)

bench-kb-redact: ## KB1.3: how long the knowledge base's redaction (SPEC §25.7) takes over 1 MiB of ordinary text, optimised (it runs over every transcript and file)
	$(CARGO) test --profile local -p ptyro-core --lib kb::redact::tests::a_mebibyte -- --nocapture

bench-kb-offer: ## KB5: how long the search behind what is offered at a prompt (SPEC §25.9) takes, optimised, over 20,000 made-up sessions (80,000 pieces of common words) for 200 prompts: the median, p95, the slowest, and how many passed the 150 ms cut-off (half a minute)
	$(CARGO) test --profile local -p ptyro-daemon --lib kb::offer::tests::the_search_behind_an_offer_at_size -- --exact --ignored --nocapture

kb-eval: ## KB1.6: the knowledge base's golden set (SPEC §25.16): 300 made-up items and chats, 63 questions through the real ingest and search; prints recall at 5 and the mean reciprocal rank by words and with meaning (a synthetic model, no download; a second), and fails when one drops two points under the numbers committed in crates/ptyro-daemon/src/kb/golden.rs or something that must never be found is (KB_EVAL_VERBOSE=1 shows every question; `make test` runs it too)
	$(CARGO) test -p ptyro-daemon --lib kb::golden::the_golden_set -- --exact --nocapture

kb-eval-real: ## KB1.6: the golden set with the real model (potion-base-8M) where it is already on this machine: PTYRO_KB_EVAL_MODEL=<folder> (default: ptyro's own data folder); it reports and downloads nothing, and says so when the model isn't there
	PTYRO_KB_EVAL_MODEL="$${PTYRO_KB_EVAL_MODEL:-$${XDG_DATA_HOME:-$$HOME/.local/share}/ptyro/models/potion-base-8M}" $(CARGO) test -p ptyro-daemon --lib kb::golden::the_golden_set_with_the_real_model -- --exact --ignored --nocapture

spike-chat: ## CHAT0: try the chat engine's claims against the real Claude, Codex and OpenCode here (ARGS="claude codex" for some; a few cents of tokens)
	python3 scripts/spike-chat.py $(ARGS)

spike-wsl: build ## WIN0.1: does WSL keep the daemon alive after the last terminal closes? Run inside WSL2: ARGS="start [label]", then (an hour later) check, clean; none = status
	scripts/spike-wsl.sh $(ARGS)


test-wsl: ## CI's main job (make check) inside WSL2: run inside the distro, or on Windows (Git Bash) with DISTRO=… WSL_DIR=… (default ~/dev/ptyro)
	DISTRO='$(DISTRO)' WSL_DIR='$(WSL_DIR)' sh scripts/test-wsl.sh

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

icons: ## Render the web app's favicon, PWA and apple-touch icons from docs/logos/ptyro-icon.svg (needs rsvg-convert)
	scripts/icons.sh

web-test: ## The web app's logic tests that need no daemon (needs Node)
	node web/test/config.mjs units
	node web/test/term.mjs units
	node web/test/outbox.mjs
	node web/test/predict.mjs
	node web/test/qr.mjs
	node web/test/kit.mjs
	node web/test/machine.mjs
	node web/test/scope.mjs
	node web/test/model.mjs
	node web/test/shells.mjs
	node web/test/chats.mjs
	node web/test/library.mjs
	node web/test/home.mjs
	node web/test/assets.mjs
	node web/test/setup.mjs

web-e2e: ## The web app in a real headless Chrome (needs Node and Chrome; PTYRO_REQUIRE_BROWSER=1 fails instead of skipping; PTYRO_E2E_ONLY="rename keep" runs only those)
	$(CARGO) test -p ptyro --test web_e2e -- --nocapture

web-check: ## Check that the committed web/dist matches web/src (CI)
	TSC="$(TSC)" scripts/web-check.sh

verify-shells: ## SH.0: press Alt-1 and see whether this terminal sends Alt (SHELLS' Alt-1…9) or what to change
	scripts/verify-shells.sh

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

dist-check: ## Install the tarball in dist/ with scripts/install.sh into a temp prefix, run it, and uninstall it with --uninstall (after `make dist`)
	TARGET=$(TRIPLE) scripts/installer-check.sh

brew-check: ## Install the tarball in dist/ through the Homebrew formula from a throwaway tap, test and audit it (after `make dist`; PTYRO_REQUIRE_BREW=1 fails without brew)
	TARGET=$(TRIPLE) scripts/brew-check.sh

installer-check: dist dist-check ## Build this platform's tarball and prove the installer script installs it

# Bundled C (SQLite, zstd, criterion's alloca) would need Apple's SDK, but `cargo check` never
# links, so a stand-in compiler (scripts/check-cc.sh) lets the whole workspace be checked.
check-macos: ## Type-check the whole workspace for macOS from Linux (CI builds everything on macOS)
	rustup target add aarch64-apple-darwin
	CC_aarch64_apple_darwin=$(CURDIR)/scripts/check-cc.sh AR_aarch64_apple_darwin=ar \
	  $(CARGO) check --workspace --all-targets --target aarch64-apple-darwin
