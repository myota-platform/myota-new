# The core service's own tasks.
#
# `make -C apps/core test` is the command issue A1's Verify block names, and it runs
# the same thing the repository gate runs for this crate. Kept as a Makefile rather
# than left as `cargo test` so the command in the issue stays stable if the
# invocation ever needs another flag.

SHELL := /bin/bash
.DEFAULT_GOAL := help

.PHONY: test
test:
	@echo "==> cargo test"
	@cargo test
	@if [ -z "$${CORE_TEST_DATABASE_URL:-}" ]; then \
	  echo ""; \
	  echo "  note  CORE_TEST_DATABASE_URL is not set, so the sign-in flow was NOT"; \
	  echo "        exercised against a database. The unit tests ran. CI supplies a"; \
	  echo "        postgres service, so CI always runs the whole suite."; \
	fi

.PHONY: test-db
test-db:
	@if [ -z "$${CORE_TEST_DATABASE_URL:-}" ]; then \
	  echo "CORE_TEST_DATABASE_URL is not set; point it at a PostgreSQL with the"; \
	  echo "core baselines applied. See apps/core/README.md."; exit 1; \
	fi
	@cargo test

.PHONY: fmt
fmt:
	@cargo fmt --all --check

.PHONY: lint
lint:
	@RUSTFLAGS="-D warnings" cargo clippy --all-targets --quiet
	@echo "clippy: ok"

.PHONY: check
check: fmt lint test
	@echo "core: ok"

.PHONY: build
build:
	@cargo build --release --bin core
	@echo "built target/release/core"

.PHONY: run
run:
	@cargo run --bin core

.PHONY: provision-admin
provision-admin:
	@cargo run --quiet --bin core -- provision-admin \
	  --email "$${EMAIL:?set EMAIL}" \
	  --name "$${NAME:-Administrator}" \
	  --password-stdin

.PHONY: help
help:
	@echo "MyOTA core"
	@echo ""
	@echo "  make test       cargo test   (the gate; DB tests need CORE_TEST_DATABASE_URL)"
	@echo "  make check      fmt + clippy + test"
	@echo "  make build      build target/release/core"
	@echo "  make run        serve against the environment's configuration"
	@echo ""
	@echo "  make provision-admin   read a password from stdin and seed an administrator"
	@echo ""
	@echo "Configuration is the environment; see src/config.rs and the README."
