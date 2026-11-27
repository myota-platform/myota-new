# The gateway's own tasks.
#
# `make -C apps/gateway test` is the command the issue's Verify block names, and it
# runs the same thing the repository gate runs for this module. Kept as a Makefile
# rather than left as `go test` so the command in the issue stays stable if the test
# invocation ever needs another flag.

SHELL := /bin/bash
.DEFAULT_GOAL := help

.PHONY: test
test:
	@echo "==> go test -race -count=1 ./..."
	@go test -race -count=1 ./...

.PHONY: test-verbose
test-verbose:
	@go test -race -count=1 -v ./...

# Coverage per package, with the total. Useful when deciding whether a new branch
# actually has a test rather than assuming it does.
.PHONY: cover
cover:
	@go test -race -count=1 -coverprofile=coverage.out ./... > /dev/null
	@go tool cover -func=coverage.out | tail -1
	@rm -f coverage.out

.PHONY: build
build:
	@mkdir -p bin
	@go build -trimpath -o bin/gateway .
	@echo "built bin/gateway"

.PHONY: run
run:
	@go run . 

.PHONY: fmt
fmt:
	@gofmt -l . | tee /dev/stderr | (! read -r _)

.PHONY: vet
vet:
	@go vet ./...

.PHONY: check
check: fmt vet test
	@echo "gateway: ok"

.PHONY: help
help:
	@echo "MyOTA gateway"
	@echo ""
	@echo "  make test       go test -race ./...   (the gate)"
	@echo "  make check      fmt + vet + test"
	@echo "  make cover      total coverage"
	@echo "  make build      build bin/gateway"
	@echo "  make run        run against the environment's configuration"
	@echo ""
	@echo "Configuration is the environment; see internal/config and the README."
