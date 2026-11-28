# syntax=docker/dockerfile:1.7
#
# The activity service.
#
# Same shape as the gateway's image — static Go binary, distroless runtime — because the
# build is the same and a second pattern would be a second thing to get wrong.
#
# P5 replaces the scaffold binary with the real service. The image does not change shape
# when it does: the binary is still `activity`, it still listens on 8080, and the
# Deployment that P5 adds points at this package by digest.

FROM golang:1.24-alpine AS build

WORKDIR /src

ENV CGO_ENABLED=0 \
    GOOS=linux \
    GOFLAGS=-mod=readonly

# `go.*` rather than `go.mod go.sum`: a service with no dependencies has no go.sum,
# and a COPY of a file that does not exist fails the build.
COPY apps/activity/go.* ./
RUN --mount=type=cache,target=/go/pkg/mod \
    go mod download

COPY apps/activity/ ./

RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    go build -trimpath -ldflags="-s -w" -o /out/activity .

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build /out/activity /activity

USER nonroot:nonroot

EXPOSE 8080

ENTRYPOINT ["/activity"]
