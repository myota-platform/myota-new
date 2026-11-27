package session

import (
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

// jwksHandler serves a key set and counts requests, so a test can assert that
// caching works and that a rollover does not turn every request into a fetch.
func jwksHandler(t *testing.T, keys map[string]ed25519.PublicKey, requests *atomic.Int32) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		document := map[string]any{"keys": []map[string]string{}}
		for keyID, key := range keys {
			document["keys"] = append(document["keys"].([]map[string]string), map[string]string{
				"kty": "OKP",
				"crv": "Ed25519",
				"kid": keyID,
				"x":   base64.RawURLEncoding.EncodeToString(key),
			})
		}
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(document); err != nil {
			t.Errorf("encoding the key document: %v", err)
		}
	}))
}

func TestJWKSFetchesOnceAndCaches(t *testing.T) {
	public, private := newKey(t)
	var requests atomic.Int32
	server := jwksHandler(t, map[string]ed25519.PublicKey{"k1": public}, &requests)
	defer server.Close()

	source := NewJWKS(server.URL, server.Client(), time.Hour)
	verifier := NewVerifier(source, testIssuer)
	token := signedToken(t, private, "k1", nil)

	for range 3 {
		if _, err := verifier.Verify(context.Background(), token); err != nil {
			t.Fatalf("Verify() error = %v", err)
		}
	}
	if got := requests.Load(); got != 1 {
		t.Errorf("key endpoint was called %d times for 3 requests, want 1", got)
	}
}

func TestJWKSRefetchesForAnUnknownKeyID(t *testing.T) {
	oldPublic, oldPrivate := newKey(t)
	newPublic, newPrivate := newKey(t)

	// The served set grows after the first fetch, which is what a key rollover looks
	// like from the gateway's side.
	served := map[string]ed25519.PublicKey{"old": oldPublic}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		document := map[string]any{"keys": []map[string]string{}}
		for keyID, key := range served {
			document["keys"] = append(document["keys"].([]map[string]string), map[string]string{
				"kty": "OKP", "crv": "Ed25519", "kid": keyID,
				"x": base64.RawURLEncoding.EncodeToString(key),
			})
		}
		_ = json.NewEncoder(w).Encode(document)
	}))
	defer server.Close()

	source := NewJWKS(server.URL, server.Client(), 50*time.Millisecond)
	verifier := NewVerifier(source, testIssuer)

	if _, err := verifier.Verify(context.Background(), signedToken(t, oldPrivate, "old", nil)); err != nil {
		t.Fatalf("the original key was rejected: %v", err)
	}

	served["new"] = newPublic
	// The cache is still warm, so the first attempt with the new kid misses and
	// refetches. Waiting past the TTL is not necessary: an unknown kid always
	// triggers one refresh.
	if _, err := verifier.Verify(context.Background(), signedToken(t, newPrivate, "new", nil)); err != nil {
		t.Fatalf("the rotated key was rejected: %v", err)
	}
}

func TestJWKSKeepsTheLastGoodSetWhenARefreshFails(t *testing.T) {
	public, private := newKey(t)
	var failing atomic.Bool
	var requests atomic.Int32

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		if failing.Load() {
			w.WriteHeader(http.StatusInternalServerError)
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{
			"keys": []map[string]string{{
				"kty": "OKP", "crv": "Ed25519", "kid": "k1",
				"x": base64.RawURLEncoding.EncodeToString(public),
			}},
		})
	}))
	defer server.Close()

	source := NewJWKS(server.URL, server.Client(), time.Nanosecond)
	verifier := NewVerifier(source, testIssuer)
	token := signedToken(t, private, "k1", nil)

	if _, err := verifier.Verify(context.Background(), token); err != nil {
		t.Fatalf("first Verify() error = %v", err)
	}

	failing.Store(true)
	// A zero TTL forces a refresh attempt on every call, so this exercises the
	// "refresh failed but the key is still known" path rather than the cache.
	if _, err := verifier.Verify(context.Background(), token); err != nil {
		t.Fatalf("Verify() after a failed refresh = %v, want the cached key to be used", err)
	}
}

func TestJWKSSkipsKeysItCannotUse(t *testing.T) {
	public, private := newKey(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		// An RSA key the gateway does not use, next to one it does. Ignoring the
		// foreign key is the difference between tolerating a richer document and
		// failing to start because another team published something.
		_ = json.NewEncoder(w).Encode(map[string]any{
			"keys": []map[string]string{
				{"kty": "RSA", "kid": "rsa-1", "x": "AAAA"},
				{"kty": "OKP", "crv": "Ed25519", "kid": "ed-1",
					"x": base64.RawURLEncoding.EncodeToString(public)},
			},
		})
	}))
	defer server.Close()

	verifier := NewVerifier(NewJWKS(server.URL, server.Client(), time.Hour), testIssuer)
	if _, err := verifier.Verify(context.Background(), signedToken(t, private, "ed-1", nil)); err != nil {
		t.Fatalf("Verify() error = %v", err)
	}
}

func TestJWKSWithoutAURLReportsNoKey(t *testing.T) {
	source := NewJWKS("", nil, time.Hour)
	if _, err := source.Key(context.Background(), "k1"); err == nil {
		t.Fatal("Key() with no URL returned no error")
	}
	if verifier := NewVerifier(source, testIssuer); verifier.Configured() {
		t.Error("Configured() = true with no key URL, want false")
	}
}
