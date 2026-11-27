# geodata

MyOTA geodata catalogue service.

| | |
| --- | --- |
| Language | Rust 2024 (Axum, SQLx, geozero, chunk A7) |
| Owns | myota_geo |
| Run modes | api · worker · relay (chunk F1 provides none) |
| Chunk | **A7** implements it |

**Owned database: myota_geo.** It holds entities and geometry, categories, sources and snapshots, import runs and candidates, conflation, review and provenance — and it is the **only** database with PostGIS.

No other service connects to it, and it connects to no other service's database.
That rule is the whole of ADR-0001: a service *is* the database it exclusively
owns.

## Current state

Scaffolding from chunk F1. The binary builds, prints one line and exits. There is
no HTTP listener, no database connection and no Dockerfile yet — `A7` adds
them.

```sh
cargo run     # prints the scaffold status line
```

## Layout

- `src/lib.rs` — the service library and its tests.
- `src/main.rs` — the thin binary entry point.

## What A7 adds

Presigned upload sessions, the preprocess worker, conflation scoring, the review queue and atomic promotion.

## Why it is not in the other language

The only genuinely CPU-bound component: streaming shapefile and PBF decode, reprojection and geometry validation over feature sets of hundreds of megabytes. No garbage collector to pacify while holding one is worth the toolchain, per ADR-0002.
