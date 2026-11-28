# The administration console's own tasks.
#
# `make -C web/admin build` is the command A3's Verify block names, and `make test` is
# A4's. Both are thin wrappers so the commands in the issues stay stable if the
# invocation ever needs another flag.

SHELL := /bin/bash
.DEFAULT_GOAL := help

NPM ?= npm
CHROME_PATH ?= $(HOME)/.cache/ms-playwright/chromium-1255/chrome-linux64/chrome

.PHONY: install
install:
	@$(NPM) ci

.PHONY: build
build: install
	@$(NPM) run build
	@echo "built dist/"

.PHONY: test
test: install
	@$(NPM) test

.PHONY: check
check: build
	@CHROME_PATH=$(CHROME_PATH) $(NPM) run check

.PHONY: dev
dev:
	@$(NPM) run dev

.PHONY: help
help:
	@echo "MyOTA administration console"
	@echo ""
	@echo "  make build   npm ci && vite build  → dist/"
	@echo "  make test    vitest (the client's rules)"
	@echo "  make check   build, then Lighthouse accessibility and the same-origin assertion"
	@echo "  make dev     vite dev server"
