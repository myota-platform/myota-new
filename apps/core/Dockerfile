# syntax=docker/dockerfile:1.7
#
# The core service.
#
# Rust links glibc by default, so the runtime is `debian:bookworm-slim` rather than
# distroless: a musl target would make the runtime smaller and the build slower and more
# likely to fail on a crate that assumes glibc. The builder and the runtime are the same
# Debian release, so the binary's libc expectation is the one it finds.
#
# `--locked` is the important flag: it fails the build when `Cargo.lock` disagrees with
# `Cargo.toml`, so the image cannot contain a dependency version the repository did not
# commit.
#
# `--bin core` names the binary explicitly. The package is `myota-core` (prefixed because a
# crate whose library is named `core` would shadow the standard library's), but the binary
# keeps the plain service name — that is what appears in logs, in `ps`, and in the workload
# name — so "build the package" and "take the binary" are not the same question.

FROM rust:1.85-bookworm AS build

WORKDIR /src

COPY apps/core/Cargo.toml apps/core/Cargo.lock ./
COPY apps/core/src ./src
# The contract is embedded with `include_str!`, so it is a build input, not a runtime
# file: without it the binary does not compile.
COPY apps/core/assets ./assets

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin core \
 && cp target/release/core /usr/local/bin/core

FROM debian:bookworm-slim

# CA certificates only: the service talks to PostgreSQL and to nothing else, and TLS to the
# database is a cluster-internal transport. A slim runtime keeps the CVE surface to the
# standard library.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 65532 --no-create-home --shell /usr/sbin/nologin nonroot

COPY --from=build /usr/local/bin/core /usr/local/bin/core

USER 65532:65532

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/core"]
