# libs/ops — the integration suite for the operational pattern.
#
# `make -C libs/ops test` is the command the issue's Verify block names. The integration
# tests need a real PostgreSQL, and there are three ways to provide one, tried in order:
#
#   1. OPS_TEST_DATABASE_URL   an existing server (what CI uses)
#   2. Docker                  a throwaway container, when the CLI is present
#   3. neither                 a loud failure, never a silent skip
#
# A check that silently skips is worse than one that fails: it reports green while
# asserting nothing. That is why this target fails rather than passing with no database.

SHELL := /bin/bash
.DEFAULT_GOAL := help

PG_IMAGE ?= postgres:16-alpine
PG_PORT  ?= 55432
PG_USER  ?= postgres
PG_PASSWORD ?= postgres

# The DSN the suites read. Populated by whichever path below can supply a server.
export OPS_TEST_DATABASE_URL

.PHONY: test
test:
	@set -euo pipefail; \
	if [ -n "$${OPS_TEST_DATABASE_URL:-}" ]; then \
	  echo "==> using OPS_TEST_DATABASE_URL"; \
	  $(MAKE) --no-print-directory test-go test-rust; \
	  exit 0; \
	fi; \
	if command -v docker >/dev/null 2>&1; then \
	  echo "==> starting $(PG_IMAGE) on port $(PG_PORT)"; \
	  container=$$(docker run -d --rm -e POSTGRES_PASSWORD=$(PG_PASSWORD) -p $(PG_PORT):5432 $(PG_IMAGE)); \
	  trap 'docker rm -f $$container >/dev/null 2>&1 || true' EXIT; \
	  for attempt in $$(seq 1 30); do \
	    if docker exec "$$container" pg_isready -U $(PG_USER) >/dev/null 2>&1; then break; fi; \
	    sleep 1; \
	  done; \
	  export OPS_TEST_DATABASE_URL="postgres://$(PG_USER):$(PG_PASSWORD)@127.0.0.1:$(PG_PORT)/postgres?sslmode=disable"; \
	  $(MAKE) --no-print-directory test-go test-rust; \
	  exit 0; \
	fi; \
	echo "ERROR: no PostgreSQL available for the integration suite." >&2; \
	echo "" >&2; \
	echo "  Set OPS_TEST_DATABASE_URL to a server the tests may create databases on," >&2; \
	echo "  or install Docker so this target can start a throwaway one." >&2; \
	echo "" >&2; \
	echo "  This target fails rather than skipping: a suite that silently asserts" >&2; \
	echo "  nothing is worse than one that says it could not run." >&2; \
	exit 1

.PHONY: test-go
test-go:
	@echo "==> go test -race -tags integration (libs/ops/go)"
	@cd go && go test -race -count=1 -tags integration ./...

.PHONY: test-rust
test-rust:
	@echo "==> cargo test (libs/ops/rust)"
	@cd rust && cargo test --quiet

# The conformance suite alone, both languages. Useful after editing a fixture: it is the
# one suite that must pass on both sides for the same reasons.
.PHONY: conformance
conformance:
	@$(MAKE) --no-print-directory -C .. test

.PHONY: fmt
fmt:
	@cd go && gofmt -l .
	@cd rust && cargo fmt --check

.PHONY: lint
lint:
	@cd go && go vet -tags integration ./...
	@cd rust && cargo clippy --all-targets --quiet -- -D warnings

.PHONY: check
check: fmt lint test

.PHONY: help
help:
	@echo "libs/ops"
	@echo ""
	@echo "  make test      integration + conformance, Go and Rust  (the gate)"
	@echo "  make test-go    Go only"
	@echo "  make test-rust  Rust only"
	@echo "  make fmt       gofmt and cargo fmt --check"
	@echo "  make lint      go vet and clippy -D warnings"
	@echo ""
	@echo "  Needs a PostgreSQL: OPS_TEST_DATABASE_URL, or Docker to start one."
