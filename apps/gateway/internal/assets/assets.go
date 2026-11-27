// Package assets embeds the files the gateway serves from its own binary.
//
// Three things live here and nothing else does:
//
//	openapi/openapi.json       the composed contract, generated from contracts/
//	events/                    one JSON Schema per event type
//	playground/standalone.js   the vendored Scalar bundle
//
// The contract and the event schemas are copies, refreshed by
// `tools/gen_gateway_assets.py`; `make verify` fails when a copy is stale. They are
// copied rather than read at runtime because `go:embed` cannot reach outside the
// module, and embedded rather than mounted because a gateway that needs a volume to
// serve its own API documentation is a gateway with a deployment dependency it does
// not need.
//
// The copies are committed rather than generated during the image build, so the
// binary that runs in the cluster is built from a contract somebody reviewed.
package assets

import "embed"

// FS holds every embedded file. The three directories are listed explicitly rather
// than embedding `.`, so a file added to this package by accident does not silently
// become part of the binary.
//
//go:embed openapi events playground
var FS embed.FS
