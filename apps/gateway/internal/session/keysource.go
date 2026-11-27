package session

import (
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sync"
	"time"
)

// KeySource resolves a key id to the public key that should have signed a token.
//
// An empty kid is passed when the token carries none: a single-key source may
// answer for it, a multi-key source should not guess.
type KeySource interface {
	Key(ctx context.Context, keyID string) (ed25519.PublicKey, error)
}

// StaticKeys is a fixed set of keys, keyed by kid. Used by tests, and by a local
// run where `core` is not up yet.
//
// The empty string key is the single-key case: a map with one entry under "" answers
// any token, including one with no kid.
type StaticKeys map[string]ed25519.PublicKey

// Key implements KeySource.
func (s StaticKeys) Key(_ context.Context, keyID string) (ed25519.PublicKey, error) {
	if key, ok := s[keyID]; ok {
		return key, nil
	}
	// A token with no kid is unambiguous when there is exactly one key to try.
	if keyID == "" && len(s) == 1 {
		for _, key := range s {
			return key, nil
		}
	}
	if len(s) == 0 {
		return nil, ErrNoKey
	}
	return nil, fmt.Errorf("%w: no key with id %q", ErrNoKey, keyID)
}

// jwksDocument is the subset of RFC 7517 the gateway understands. Anything else in
// a JWKS — RSA keys, key operations, X.509 chains — is ignored rather than rejected:
// a future `core` may publish keys for another purpose in the same document.
type jwksDocument struct {
	Keys []struct {
		KeyType   string `json:"kty"`
		Curve     string `json:"crv"`
		KeyID     string `json:"kid"`
		PublicKey string `json:"x"`
	} `json:"keys"`
}

// JWKS fetches and caches `core`'s published key set.
//
// Two behaviours matter and are easy to get wrong:
//
//   - **It caches, and it caches on success only.** A failed fetch leaves the last
//     good set in place, so a brief `core` restart does not start refusing tokens.
//   - **It refetches on an unknown kid**, at most once per TTL. That is what makes a
//     key rollover seamless: new tokens carry a new kid, the gateway does not know
//     it, asks once, and carries on. Without the TTL bound, a caller presenting a
//     random kid would turn every request into an outbound fetch.
type JWKS struct {
	url    string
	client *http.Client
	ttl    time.Duration
	now    func() time.Time

	mutex     sync.Mutex
	keys      StaticKeys
	fetchedAt time.Time
}

// NewJWKS returns a source that reads url. The first fetch happens on demand, not at
// construction, so the gateway can start before `core` does.
func NewJWKS(url string, client *http.Client, ttl time.Duration) *JWKS {
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Second}
	}
	return &JWKS{url: url, client: client, ttl: ttl, now: time.Now}
}

// Key implements KeySource.
func (j *JWKS) Key(ctx context.Context, keyID string) (ed25519.PublicKey, error) {
	j.mutex.Lock()
	defer j.mutex.Unlock()

	if key, ok := j.keys[keyID]; ok && j.fresh() {
		return key, nil
	}
	// Either the set is stale, empty, or the kid is unknown. One fetch answers all
	// three; the TTL check happens after it to bound the retry rate.
	if err := j.fetchLocked(ctx); err != nil {
		if key, ok := j.keys[keyID]; ok {
			// Serving a stale key beats refusing every request because one refresh
			// failed. A rotated-out key is still refused, because it is not in the
			// set we last saw.
			return key, nil
		}
		return nil, err
	}
	if key, ok := j.keys[keyID]; ok {
		return key, nil
	}
	if keyID == "" && len(j.keys) == 1 {
		for _, key := range j.keys {
			return key, nil
		}
	}
	return nil, fmt.Errorf("%w: no key with id %q after refresh", ErrNoKey, keyID)
}

// fresh reports whether the cached set is recent enough to answer an unknown kid.
// Callers must hold the mutex.
func (j *JWKS) fresh() bool {
	return j.now().Sub(j.fetchedAt) < j.ttl
}

// fetchLocked reads the key set. Callers must hold the mutex, which serialises
// concurrent refreshes so a burst of unknown kids produces one request.
func (j *JWKS) fetchLocked(ctx context.Context) error {
	if j.url == "" {
		return ErrNoKey
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, j.url, nil)
	if err != nil {
		return fmt.Errorf("session: building key request: %w", err)
	}
	response, err := j.client.Do(request)
	if err != nil {
		return fmt.Errorf("session: fetching keys: %w", err)
	}
	defer func() { _ = response.Body.Close() }()

	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("session: key endpoint returned %s", response.Status)
	}
	// Bounded read: a key document is a few hundred bytes, and an unbounded read of
	// an internal endpoint is how a compromised or misconfigured peer exhausts the
	// gateway's memory.
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return fmt.Errorf("session: reading keys: %w", err)
	}

	var document jwksDocument
	if err := json.Unmarshal(body, &document); err != nil {
		return fmt.Errorf("session: parsing keys: %w", err)
	}

	parsed := make(StaticKeys, len(document.Keys))
	for _, entry := range document.Keys {
		// Only Ed25519 is usable here; the verifier pins EdDSA, so another key type
		// in the document is not an error, it is just not a session key.
		if entry.KeyType != "OKP" || entry.Curve != "Ed25519" {
			continue
		}
		raw, err := base64.RawURLEncoding.DecodeString(entry.PublicKey)
		if err != nil {
			return fmt.Errorf("session: decoding key %q: %w", entry.KeyID, err)
		}
		if len(raw) != ed25519.PublicKeySize {
			return fmt.Errorf("session: key %q is %d bytes, want %d", entry.KeyID, len(raw), ed25519.PublicKeySize)
		}
		parsed[entry.KeyID] = ed25519.PublicKey(raw)
	}
	if len(parsed) == 0 {
		return fmt.Errorf("%w: key document contained no Ed25519 keys", ErrNoKey)
	}

	j.keys = parsed
	j.fetchedAt = j.now()
	return nil
}
