// Command gateway is the MyOTA API gateway.
//
// One binary, no database, no state. It does exactly four things:
//
//   - routes a request to the service that owns its prefix, using a table generated
//     from the contract;
//   - serves the composed contract at /openapi.json and the Scalar reference at
//     /docs, both from its own binary;
//   - enforces the administrator audience before a request reaches a service;
//   - reports its own health and metrics.
//
// It does not aggregate, retry, cache, rate limit or terminate TLS. Rate limiting is
// Traefik's (ADR-0005), because a counter in a replica multiplies the configured
// limit by the replica count. TLS is Traefik's too. Aggregation is nobody's: a screen
// that needs two services makes two calls.
//
// Configuration is the environment (internal/config). There is no secret in it: the
// token verification key is fetched from `core` (ADR-0008).
package main

import (
	"context"
	"crypto/ed25519"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/myota-platform/myota-new/apps/gateway/internal/assets"
	"github.com/myota-platform/myota-new/apps/gateway/internal/config"
	"github.com/myota-platform/myota-new/apps/gateway/internal/gateway"
	"github.com/myota-platform/myota-new/apps/gateway/internal/playground"
	"github.com/myota-platform/myota-new/apps/gateway/internal/session"
	"github.com/myota-platform/myota-new/apps/gateway/internal/upstream"
)

// shutdownGrace bounds how long a draining pod waits for in-flight requests.
const shutdownGrace = 20 * time.Second

func main() {
	slog.SetDefault(slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo})))

	if err := run(); err != nil {
		slog.Error("gateway stopped", "error", err)
		os.Exit(1)
	}
}

// run builds the server, serves until a signal, and drains. It returns an error
// rather than exiting so the failure path is one place in main.
func run() error {
	configuration, err := config.Load()
	if err != nil {
		return err
	}

	keySource, err := buildKeySource(configuration)
	if err != nil {
		return err
	}
	verifier := session.NewVerifier(keySource, configuration.JWTIssuer)
	if !verifier.Configured() {
		// Not fatal: the public API works without verification, and a pod that refuses
		// to start because `core` has not published its keys yet would invent a startup
		// ordering dependency for no benefit. /readyz reports it, and every
		// administrator route answers 401 until it is fixed.
		slog.Warn("no token verification key; administrator routes will refuse every request",
			"jwks_url", configuration.JWKSFromCoreURL(), "issuer", configuration.JWTIssuer)
	}

	upstreams, err := upstream.NewSet(configuration.Upstreams, configuration.RequestTimeout, nil)
	if err != nil {
		return err
	}

	reference, err := playground.New(assets.FS)
	if err != nil {
		return err
	}

	service := gateway.New(configuration, upstreams, verifier, reference, gateway.NewMetrics())

	server := &http.Server{
		Addr:    configuration.ListenAddress,
		Handler: service.Handler(),
		// Timeouts on the edge itself. Without ReadHeaderTimeout a slow-loris client
		// holds a connection and a goroutine for as long as it likes; without
		// IdleTimeout an idle keep-alive connection does the same.
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      configuration.RequestTimeout + 10*time.Second,
		IdleTimeout:       90 * time.Second,
		MaxHeaderBytes:    32 << 10,
	}

	slog.Info("gateway listening",
		"address", configuration.ListenAddress,
		"upstreams", upstreams.Names(),
		"request_timeout", configuration.RequestTimeout.String(),
		"jwks_url", configuration.JWKSFromCoreURL())

	// Shutdown on SIGTERM: the kubelet sends it when it drains a pod, and finishing
	// in-flight requests before closing is the difference between a rolling update and
	// a handful of 502s.
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGINT, syscall.SIGTERM)

	serveErr := make(chan error, 1)
	go func() {
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			serveErr <- fmt.Errorf("serving: %w", err)
			return
		}
		serveErr <- nil
	}()

	select {
	case err := <-serveErr:
		return err
	case received := <-signals:
		slog.Info("draining", "signal", received.String(), "grace", shutdownGrace.String())
	}

	ctx, cancel := context.WithTimeout(context.Background(), shutdownGrace)
	defer cancel()
	if err := server.Shutdown(ctx); err != nil {
		return fmt.Errorf("draining: %w", err)
	}
	slog.Info("stopped")
	return nil
}

// buildKeySource picks where verification keys come from.
//
// Two sources, one rule: never invent a key. A static key file exists for a local run
// and for tests, where `core` is not up; in a deployment the JWKS over the internal
// network is the only source. Both fail closed — an empty source refuses every
// administrator request rather than accepting an unverified token.
func buildKeySource(configuration config.Config) (session.KeySource, error) {
	if configuration.JWTPublicKeyFile != "" {
		key, err := loadPublicKeyFile(configuration.JWTPublicKeyFile)
		if err != nil {
			return nil, err
		}
		slog.Info("using a static verification key",
			"file", configuration.JWTPublicKeyFile, "kid", configuration.JWTKeyID)
		return session.StaticKeys{configuration.JWTKeyID: key}, nil
	}
	return session.NewJWKS(configuration.JWKSFromCoreURL(), nil, configuration.JWKSCacheTTL), nil
}

// loadPublicKeyFile reads an Ed25519 public key.
//
// PEM (PKIX, what `openssl` and most tooling write) or a bare base64 value in the
// form the JWKS uses. Accepting both is worth four lines: the base64 form is what a
// developer copies out of `core`'s key document and the PEM form is what they get out
// of openssl, and refusing one of them produces a support question rather than a
// security improvement.
func loadPublicKeyFile(path string) (ed25519.PublicKey, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("gateway: reading the verification key: %w", err)
	}
	content := strings.TrimSpace(string(raw))

	if block, _ := pem.Decode(raw); block != nil {
		parsed, err := x509.ParsePKIXPublicKey(block.Bytes)
		if err != nil {
			return nil, fmt.Errorf("gateway: parsing the PEM verification key: %w", err)
		}
		key, ok := parsed.(ed25519.PublicKey)
		if !ok {
			return nil, fmt.Errorf("gateway: %s holds a %T, not an Ed25519 public key", path, parsed)
		}
		return key, nil
	}

	decoded, err := base64.RawURLEncoding.DecodeString(content)
	if err != nil {
		return nil, fmt.Errorf("gateway: %s is neither PEM nor base64url: %w", path, err)
	}
	if len(decoded) != ed25519.PublicKeySize {
		return nil, fmt.Errorf("gateway: %s decodes to %d bytes, want %d",
			path, len(decoded), ed25519.PublicKeySize)
	}
	return ed25519.PublicKey(decoded), nil
}
