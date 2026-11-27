package config

import (
	"strings"
	"testing"
	"time"
)

// lookupFrom builds an environment lookup from a map.
func lookupFrom(environment map[string]string) func(string) string {
	return func(key string) string { return environment[key] }
}

// requiredUpstreams is the minimum a configuration needs: every service the route
// table names must have a base URL.
func requiredUpstreams() map[string]string {
	return map[string]string{
		"GATEWAY_UPSTREAM_CORE":     "http://core-api:8080",
		"GATEWAY_UPSTREAM_GEODATA":  "http://geodata-api:8080",
		"GATEWAY_UPSTREAM_ACTIVITY": "http://activity-api:8080",
	}
}

func TestLoadFromAppliesDefaults(t *testing.T) {
	configuration, err := LoadFrom(lookupFrom(requiredUpstreams()))
	if err != nil {
		t.Fatalf("LoadFrom: %v", err)
	}
	if configuration.ListenAddress != DefaultListenAddress {
		t.Errorf("ListenAddress = %q, want %q", configuration.ListenAddress, DefaultListenAddress)
	}
	if configuration.RequestTimeout != DefaultRequestTimeout {
		t.Errorf("RequestTimeout = %s, want %s", configuration.RequestTimeout, DefaultRequestTimeout)
	}
	if configuration.JWTIssuer != DefaultSessionIssuer {
		t.Errorf("JWTIssuer = %q, want %q", configuration.JWTIssuer, DefaultSessionIssuer)
	}
	if len(configuration.Upstreams) != 3 {
		t.Errorf("Upstreams = %v, want three", configuration.Upstreams)
	}
}

func TestLoadFromRequiresEveryServiceInTheRouteTable(t *testing.T) {
	// Every name the generated table routes to must be configured, because a missing
	// one is a 500 on the first request to that prefix rather than a startup failure.
	environment := requiredUpstreams()
	delete(environment, "GATEWAY_UPSTREAM_GEODATA")

	_, err := LoadFrom(lookupFrom(environment))
	if err == nil {
		t.Fatal("LoadFrom accepted a configuration with no geodata upstream")
	}
	if !strings.Contains(err.Error(), "GATEWAY_UPSTREAM_GEODATA") {
		t.Errorf("error = %v, want it to name the missing variable", err)
	}
}

func TestLoadFromRejectsAnUnparseableTimeout(t *testing.T) {
	environment := requiredUpstreams()
	environment[requestTimeoutEnv] = "ten seconds"

	if _, err := LoadFrom(lookupFrom(environment)); err == nil {
		t.Fatal("LoadFrom accepted a timeout that is not a duration")
	}

	environment[requestTimeoutEnv] = "-5s"
	if _, err := LoadFrom(lookupFrom(environment)); err == nil {
		t.Fatal("LoadFrom accepted a negative timeout")
	}
}

func TestLoadFromReadsAnExplicitTimeout(t *testing.T) {
	environment := requiredUpstreams()
	environment[requestTimeoutEnv] = "3s"

	configuration, err := LoadFrom(lookupFrom(environment))
	if err != nil {
		t.Fatalf("LoadFrom: %v", err)
	}
	if configuration.RequestTimeout != 3*time.Second {
		t.Errorf("RequestTimeout = %s, want 3s", configuration.RequestTimeout)
	}
}

func TestJWKSURLIsDerivedFromCore(t *testing.T) {
	// Derived rather than configured separately, so the gateway cannot be pointed at
	// one core for traffic and another for keys.
	configuration, err := LoadFrom(lookupFrom(requiredUpstreams()))
	if err != nil {
		t.Fatalf("LoadFrom: %v", err)
	}
	if got, want := configuration.JWKSFromCoreURL(), "http://core-api:8080/internal/auth/jwks"; got != want {
		t.Errorf("JWKSFromCoreURL() = %q, want %q", got, want)
	}

	environment := requiredUpstreams()
	environment[jwksURLEnv] = "http://elsewhere/jwks"
	overridden, err := LoadFrom(lookupFrom(environment))
	if err != nil {
		t.Fatalf("LoadFrom: %v", err)
	}
	if got := overridden.JWKSFromCoreURL(); got != "http://elsewhere/jwks" {
		t.Errorf("JWKSFromCoreURL() = %q, want the explicit override", got)
	}
}

func TestRejectUnknownUpstreams(t *testing.T) {
	// A typo like GATEWAY_UPSTREAM_GEODATA_API is almost always a mistake for the real
	// name, and ignoring it leaves the intended service pointed nowhere.
	environ := []string{
		"GATEWAY_UPSTREAM_CORE=http://core",
		"GATEWAY_UPSTREAM_GEODATA=http://geodata",
		"GATEWAY_UPSTREAM_ACTIVITY=http://activity",
	}
	if err := rejectUnknownUpstreams(environ); err != nil {
		t.Errorf("rejectUnknownUpstreams(valid) = %v, want nil", err)
	}

	if err := rejectUnknownUpstreams(append(environ, "GATEWAY_UPSTREAM_GEODATA_API=http://x")); err == nil {
		t.Fatal("an unknown upstream variable was accepted")
	}
}
