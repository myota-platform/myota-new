# MyOTA — repository tasks.
#
# `make verify` is the gate: it runs everything CI runs for languages and static
# checks. A target fails loudly when its toolchain is missing rather than skipping,
# because a verification gate that silently skips is the false-pass problem
# recorded in ADR-0006.
#
# Toolchains: Go 1.24+, Rust 1.85+ with rustfmt and clippy, Python 3.11+, Node 20+.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# Modules and crates are discovered rather than listed, so a new service joins the
# gate by existing. A hand-maintained list is a list that goes stale.
GO_MODULE_DIRS    := $(patsubst %/go.mod,%,$(shell find . -name go.mod -not -path '*/node_modules/*' -not -path '*/target/*'))
RUST_CRATE_DIRS   := $(patsubst %/Cargo.toml,%,$(shell find . -name Cargo.toml -not -path '*/node_modules/*' -not -path '*/target/*'))

# Checks that take no arguments are discovered, so a new one joins the gate by
# existing. The two that need an argument or a flag are named below.
PYTHON_CHECKS := $(filter-out tools/check_design_system.py tools/check_diagrams.py tools/check_db_schema.py tools/check_deploy.py tools/check_fleet_bundles.py,$(wildcard tools/check_*.py))
NODE_CHECKS   := tools/check_ui.mjs

# ---------------------------------------------------------------- toolchains ---

.PHONY: check-toolchains
check-toolchains:
	@missing=0; \
	for tool in go cargo rustc python3 node; do \
	  if ! command -v $$tool >/dev/null 2>&1; then \
	    echo "MISSING: $$tool"; missing=1; \
	  fi; \
	done; \
	if [ "$$missing" = "1" ]; then \
	  echo ""; \
	  echo "Install the missing toolchains:"; \
	  echo "  Go 1.24+      https://go.dev/dl/"; \
	  echo "  Rust 1.85+    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; \
	  echo "                then: rustup component add rustfmt clippy"; \
	  exit 1; \
	fi
	@cargo fmt --version >/dev/null 2>&1 || { echo "MISSING: rustfmt — run 'rustup component add rustfmt'"; exit 1; }
	@cargo clippy --version >/dev/null 2>&1 || { echo "MISSING: clippy — run 'rustup component add clippy'"; exit 1; }

# JavaScript tooling is only needed for the browser-dependent checks. Installed on
# demand so a clean checkout can run `make verify` without a separate setup step.
node_modules/.package-lock.json: package-lock.json package.json
	@echo "==> npm ci (repository tooling)"
	npm ci

# ------------------------------------------------------------------- targets ---

.PHONY: fmt
fmt: check-toolchains
	@echo "==> gofmt"
	@unformatted="$$(gofmt -l $(GO_MODULE_DIRS))"; \
	if [ -n "$$unformatted" ]; then \
	  echo "gofmt needed:"; echo "$$unformatted"; exit 1; \
	fi
	@for dir in $(RUST_CRATE_DIRS); do \
	  echo "==> cargo fmt --check ($$dir)"; \
	  ( cd "$$dir" && cargo fmt --all --check ) || exit 1; \
	done

.PHONY: lint
lint: check-toolchains
	@for dir in $(GO_MODULE_DIRS); do \
	  echo "==> go vet ($$dir)"; \
	  ( cd "$$dir" && go vet ./... ) || exit 1; \
	done
	@for dir in $(RUST_CRATE_DIRS); do \
	  echo "==> cargo clippy ($$dir)"; \
	  ( cd "$$dir" && RUSTFLAGS="-D warnings" cargo clippy --all-targets --quiet ) || exit 1; \
	done

.PHONY: test
test: check-toolchains
	@for dir in $(GO_MODULE_DIRS); do \
	  echo "==> go test ($$dir)"; \
	  ( cd "$$dir" && go test -race -count=1 ./... ) || exit 1; \
	done
	@for dir in $(RUST_CRATE_DIRS); do \
	  echo "==> cargo test ($$dir)"; \
	  ( cd "$$dir" && cargo test --quiet ) || exit 1; \
	done

# The contract gate: compose the three service documents, validate the result as
# OpenAPI 3.1, and fail when the committed composed file is stale. It needs PyYAML
# and — for the real validation rather than the structural fallback — the packages
# in tools/requirements.txt.
.PHONY: checks-contracts
checks-contracts:
	@echo "==> tools/compose_contracts.py --check"
	@python3 tools/compose_contracts.py --check
	@echo "==> tools/test_compose_contracts.py"
	@python3 tools/test_compose_contracts.py
	@echo "==> tools/gen_gateway_assets.py --check"
	@python3 tools/gen_gateway_assets.py --check
	@echo "==> tools/gen_gateway_routes.py --check"
	@python3 tools/gen_gateway_routes.py --check
	@echo "==> tools/gen_ops_sql.py --check"
	@python3 tools/gen_ops_sql.py --check

# Static checks that need no browser: decision records, links, the event
# catalogue, and design-system conformance for any front end that exists.
.PHONY: checks-static
checks-static:
	@for check in $(PYTHON_CHECKS); do \
	  echo "==> $$check"; python3 "$$check" || exit 1; \
	done
	@if [ -d web/admin ]; then echo "==> design system (web/admin)"; python3 tools/check_design_system.py web/admin; fi
	@if [ -d web/participant ]; then echo "==> design system (web/participant)"; python3 tools/check_design_system.py web/participant; fi
	@echo "==> tools/provision_silo.py --check"
	@python3 tools/provision_silo.py --check

# Browser-dependent checks. Kept separate so `make verify` is usable on a machine
# without a headless browser; `make verify-full` adds these and CI runs both.
.PHONY: checks-browser
checks-browser: node_modules/.package-lock.json
	@for check in $(NODE_CHECKS); do \
	  echo "==> $$check"; node "$$check" || exit 1; \
	done
	@echo "==> tools/render_mockups.mjs --check"
	@node tools/render_mockups.mjs --check
	@echo "==> tools/check_playground.mjs"
	@node tools/check_playground.mjs

# Diagrams: heuristics only. --full additionally compiles the Structurizr DSL and
# renders every Mermaid block, which needs a JRE, structurizr-cli and mmdc — the
# CI docs job provides those.
.PHONY: checks-diagrams
checks-diagrams:
	@echo "==> tools/check_diagrams.py"
	@python3 tools/check_diagrams.py

# The ops integration suite: outbox, relay, job claim, idempotency, migrations. It needs a
# real PostgreSQL, so unlike the rest of `verify` it cannot run on a bare checkout.
#
# `checks-ops` reports when it cannot run and passes — the same rule as a missing `mmdc`.
# `make -C libs/ops test` is the strict form and fails loudly; that is the one CI uses and
# the one the issue's Verify block names.
.PHONY: checks-ops
checks-ops:
	@if [ -n "$${OPS_TEST_DATABASE_URL:-}" ] || command -v docker >/dev/null 2>&1; then \
	  echo "==> make -C libs/ops test"; \
	  $(MAKE) --no-print-directory -C libs/ops test; \
	else \
	  echo "==> libs/ops integration suite"; \
	  echo "  note  no OPS_TEST_DATABASE_URL and no docker — the ops integration tests"; \
	  echo "        were NOT run. 'make -C libs/ops test' fails loudly in this state;"; \
	  echo "        CI supplies a postgres service, so CI always runs them."; \
	fi

# The deployment: three databases, their baselines and the chart that runs them.
#
# `check_db_schema` is offline and always runs. `check_deploy` renders the chart with helm,
# so it reports and passes when helm is absent — the same rule as a missing `mmdc`. CI
# installs helm, so CI always renders.
.PHONY: checks-deploy
checks-deploy:
	@echo "==> tools/check_db_schema.py --check"
	@python3 tools/check_db_schema.py --check
	@echo "==> tools/check_fleet_bundles.py --check"
	@python3 tools/check_fleet_bundles.py --check
	@if command -v helm >/dev/null 2>&1; then \
	  echo "==> tools/check_deploy.py --check"; \
	  python3 tools/check_deploy.py --check; \
	else \
	  echo "==> tools/check_deploy.py"; \
	  echo "  note  helm not found — the chart was NOT rendered. Install helm"; \
	  echo "        to check the pooler-only rule, the connection budget and the"; \
	  echo "        migration Job locally; CI always does."; \
	fi

.PHONY: verify
verify: check-toolchains fmt lint test checks-contracts checks-deploy checks-static checks-diagrams
	@echo ""
	@echo "verify: ok"

.PHONY: verify-full
verify-full: verify checks-ops checks-browser
	@echo ""
	@echo "verify-full: ok  (browser and diagrams rendering included)"

.PHONY: clean
clean:
	@for dir in $(RUST_CRATE_DIRS); do ( cd "$$dir" && cargo clean --quiet ); done
	@find $(GO_MODULE_DIRS) -maxdepth 1 -type d -name bin -exec rm -rf {} +

.PHONY: help
help:
	@echo "MyOTA repository tasks"
	@echo ""
	@echo "  make verify       fmt + lint + test + static and diagram checks  (the gate)"
	@echo "  make verify-full  verify + ops integration + browser checks"
	@echo "  make fmt          gofmt and cargo fmt --check"
	@echo "  make lint         go vet and cargo clippy -D warnings"
	@echo "  make test         go test -race and cargo test, per module and crate"
	@echo "  make checks-static   ADRs, links, event catalogue, storage inventory, design system"
	@echo "  make checks-contracts  compose the contracts and validate OpenAPI 3.1"
	@echo "  make checks-deploy   database baselines and the rendered chart"
	@echo "  make checks-ops     ops integration suite (needs a PostgreSQL)"
	@echo "  make checks-browser  UI sweep, mockup renders, playground renders in a browser"
	@echo "  make checks-diagrams diagram heuristics (Mermaid, Structurizr name checks)"
	@echo "  make clean        remove build output"
	@echo ""
	@echo "Discovered: $(words $(GO_MODULE_DIRS)) Go module(s), $(words $(RUST_CRATE_DIRS)) Rust crate(s)"
