# syntax=docker/dockerfile:1.7
#
# The API gateway.
#
# Two stages: the whole toolchain builds a static binary, and the runtime is distroless —
# no shell, no package manager, nothing to exec into. A gateway that terminates nothing and
# holds no credential has no reason to carry a userspace.
#
# `CGO_ENABLED=0` is what makes the distroless static base possible: no libc to link, so the
# binary runs on an image that contains nothing but the binary and the CA bundle.
#
# The build runs from the repository root as the context, so `apps/gateway` is addressed by
# path rather than assumed to be the context. That keeps one Dockerfile shape for every
# service and one `.dockerignore`.

FROM golang:1.24-alpine AS build

WORKDIR /src

# `-mod=readonly` turns "go.mod needs an update" into a build failure instead of a silent
# network fetch of a different version than the one committed.
ENV CGO_ENABLED=0 \
    GOOS=linux \
    GOFLAGS=-mod=readonly

# Dependencies first, so a source edit does not re-download the module graph.
COPY apps/gateway/go.mod apps/gateway/go.sum ./
RUN --mount=type=cache,target=/go/pkg/mod \
    go mod download

COPY apps/gateway/ ./

# `-trimpath` keeps the build reproducible and out of the binary's strings; `-s -w` drops
# the symbol table and DWARF, which nothing here needs.
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    go build -trimpath -ldflags="-s -w" -o /out/gateway .

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build /out/gateway /gateway

# The distroless nonroot tag already sets this; being explicit means a change of base image
# cannot quietly hand the process a privileged uid.
USER nonroot:nonroot

EXPOSE 8080

ENTRYPOINT ["/gateway"]
