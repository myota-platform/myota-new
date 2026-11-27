# core

MyOTA identity and programme service.

| | |
| --- | --- |
| Language | Rust 2024 (Axum, SQLx, chunk A1) |
| Owns | myota_core |
| Run modes | api · worker · relay (chunk F1 provides none) |
| Chunk | **A1** implements it |

**Owned database: myota_core.** It holds accounts, credentials, sessions, MFA, callsigns, roles, programmes, content, policy, and the single notification ledger — plus the `ops` schema.

No other service connects to it, and it connects to no other service's database.
That rule is the whole of ADR-0001: a service *is* the database it exclusively
owns.

## Current state

Scaffolding from chunk F1. The binary builds, prints one line and exits. There is
no HTTP listener, no database connection and no Dockerfile yet — `A1` adds
them.

```sh
cargo run     # prints the scaffold status line
```

## Layout

- `src/lib.rs` — the service library and its tests.
- `src/main.rs` — the thin binary entry point.

## What A1 adds

Admin login with mandatory MFA, participant login and registration, the four email flows, roles and scope introspection, programmes and policy versions.

## Why it is not in the other language

This is the surface that ends the project if it is wrong. `sqlx` verifies every query against a migrated database at compile time, and the credential path has no garbage collector to pause. ADR-0002 accepts the extra toolchain for exactly this service and `geodata`.
