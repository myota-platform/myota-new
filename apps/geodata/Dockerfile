# syntax=docker/dockerfile:1.7
#
# The geodata service.
#
# Same shape as `core`'s image: Debian bookworm builder and runtime, static-by-default Rust,
# `--locked` so the image cannot contain a dependency the repository did not commit.
#
# A7 adds PostGIS access through SQLx. That is a client library, not a native dependency, so
# the runtime stays a slim Debian image — no Postgres client tools, no GDAL.

FROM rust:1.85-bookworm AS build

WORKDIR /src

COPY apps/geodata/Cargo.toml apps/geodata/Cargo.lock ./
COPY apps/geodata/src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin geodata \
 && cp target/release/geodata /usr/local/bin/geodata

FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 65532 --no-create-home --shell /usr/sbin/nologin nonroot

COPY --from=build /usr/local/bin/geodata /usr/local/bin/geodata

USER 65532:65532

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/geodata"]
