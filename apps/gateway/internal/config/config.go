// Package config reads the gateway's configuration from the environment.
//
// Configuration only — never a secret. The gateway holds no credential: the token
// verification key is fetched from `core` over the internal network (ADR-0008), and
// database URLs do not exist here because the gateway has no database. That keeps
// this package to plain strings and one duration, and it means the gateway's
// deployment has nothing in it worth encrypting.
//
// Every variable is named explicitly and read once at startup. A configuration error
// stops the process rather than surfacing as a 500 on the first request to a prefix,
// because a gateway that starts and then fails half its traffic is harder to
// diagnose than one that refuses to start.
package config

import (
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/myota-platform/myota-new/apps/gateway/internal/routes"
)

// Defaults for the values that have a sensible one.
const (
	DefaultListenAddress  = ":8080"
	DefaultRequestTimeout = 10 * time.Second
	DefaultSessionIssuer  = "myota-core"
	jwksTTL               = 15 * time.Minute
	jwksPath              = "/internal/auth/jwks"
	upstreamEnvPrefix     = "GATEWAY_UPSTREAM_"
	listenAddressEnv      = "GATEWAY_LISTEN_ADDR"
	requestTimeoutEnv     = "GATEWAY_REQUEST_TIMEOUT"
	jwksURLEnv            = "GATEWAY_JWKS_URL"
	jwtIssuerEnv          = "GATEWAY_JWT_ISSUER"
	jwtPublicKeyFileEnv   = "GATEWAY_JWT_PUBLIC_KEY_FILE"
	jwtPublicKeyIDEnv     = "GATEWAY_JWT_KEY_ID"
)

// Config is the gateway's whole configuration.
type Config struct {
	// ListenAddress is the host:port to serve on.
	ListenAddress string
	// Upstreams maps a contract service name to its base URL.
	Upstreams map[string]string
	// RequestTimeout bounds one proxied exchange, headers and body.
	RequestTimeout time.Duration
	// JWKSURL is where `core` publishes its token verification keys.
	JWKSURL string
	// JWKSCacheTTL bounds how often an unknown key id causes a refetch.
	JWKSCacheTTL time.Duration
	// JWTIssuer is the `iss` a session token must carry.
	JWTIssuer string
	// JWTPublicKeyFile is an Ed25519 public key in PEM or raw-base64 form, for a
	// local run without `core` and for tests. Empty in a deployment.
	JWTPublicKeyFile string
	// JWTKeyID labels the key in JWTPublicKeyFile, so a token with a matching kid
	// verifies against it.
	JWTKeyID string
}

// Load reads the configuration from the process environment.
func Load() (Config, error) {
	configuration, err := LoadFrom(os.Getenv)
	if err != nil {
		return Config{}, err
	}
	if err := rejectUnknownUpstreams(os.Environ()); err != nil {
		return Config{}, err
	}
	return configuration, nil
}

// rejectUnknownUpstreams fails on a GATEWAY_UPSTREAM_* that names no service.
//
// It reads the real environment rather than the lookup function, so it lives outside
// LoadFrom and stays out of tests. A stray variable is almost always a typo for a
// real one — `GATEWAY_UPSTREAM_GEODATA_API` next to `GATEWAY_UPSTREAM_GEODATA` — and
// silently ignoring it means the service that was meant to be configured stays
// pointed at whatever the default is.
func rejectUnknownUpstreams(environ []string) error {
	for _, entry := range environ {
		name, _, _ := strings.Cut(entry, "=")
		if !strings.HasPrefix(name, upstreamEnvPrefix) {
			continue
		}
		service := strings.ToLower(strings.TrimPrefix(name, upstreamEnvPrefix))
		known := false
		for _, candidate := range routes.Upstreams() {
			if candidate == service {
				known = true
				break
			}
		}
		if !known {
			return fmt.Errorf("config: %s names no service in the route table; known names are %s",
				name, strings.Join(routes.Upstreams(), ", "))
		}
	}
	return nil
}

// LoadFrom reads the configuration using a lookup function, so tests can supply one
// without touching the process environment.
func LoadFrom(lookup func(string) string) (Config, error) {
	configuration := Config{
		ListenAddress:    valueOrDefault(lookup(listenAddressEnv), DefaultListenAddress),
		RequestTimeout:   DefaultRequestTimeout,
		JWKSURL:          lookup(jwksURLEnv),
		JWKSCacheTTL:     jwksTTL,
		JWTIssuer:        valueOrDefault(lookup(jwtIssuerEnv), DefaultSessionIssuer),
		JWTPublicKeyFile: lookup(jwtPublicKeyFileEnv),
		JWTKeyID:         lookup(jwtPublicKeyIDEnv),
	}

	if raw := lookup(requestTimeoutEnv); raw != "" {
		parsed, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("config: %s = %q is not a duration: %w", requestTimeoutEnv, raw, err)
		}
		if parsed <= 0 {
			return Config{}, fmt.Errorf("config: %s must be positive, got %s", requestTimeoutEnv, parsed)
		}
		configuration.RequestTimeout = parsed
	}

	// Only the services the contract declares. A typo in a variable name would
	// otherwise leave a service unconfigured and every request to it a 500; reading
	// the route table here makes the check exact.
	upstreams := make(map[string]string, 3)
	for _, name := range routes.Upstreams() {
		key := upstreamEnvPrefix + strings.ToUpper(name)
		raw := lookup(key)
		if raw == "" {
			return Config{}, fmt.Errorf(
				"config: %s is not set; the contract routes traffic to %q", key, name)
		}
		upstreams[name] = raw
	}
	configuration.Upstreams = upstreams

	return configuration, nil
}

// valueOrDefault returns value, or fallback when value is empty.
func valueOrDefault(value, fallback string) string {
	if value == "" {
		return fallback
	}
	return value
}

// JWKSFromCoreURL returns the JWKS endpoint for a `core` base URL.
//
// Derived from the upstream URL rather than configured separately, so the gateway
// cannot be pointed at one `core` for traffic and another for keys.
func (c Config) JWKSFromCoreURL() string {
	if c.JWKSURL != "" {
		return c.JWKSURL
	}
	coreURL, ok := c.Upstreams["core"]
	if !ok {
		return ""
	}
	return strings.TrimSuffix(coreURL, "/") + jwksPath
}
