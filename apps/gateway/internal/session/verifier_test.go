package session

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
)

const testIssuer = "myota-core"

// signedToken builds a token the way `core` does, so the tests exercise the real
// verification path rather than a mock of it.
func signedToken(t *testing.T, private ed25519.PrivateKey, keyID string, mutate func(jwt.MapClaims)) string {
	t.Helper()
	now := time.Now()
	claims := jwt.MapClaims{
		"iss": testIssuer,
		"aud": string(AudienceParticipant),
		"sub": "44a1b7c2-0d3e-4f51-9a86-2c7d1e5f0a93",
		"sid": "9b2f4d61-8c3a-4e57-b0d1-6f8a2c4e7d95",
		"jti": "5a3e9c17-2d4b-4f68-8a0e-7c1b5d9f2e34",
		"scp": []string{"geodata.review"},
		"iat": now.Unix(),
		"exp": now.Add(15 * time.Minute).Unix(),
	}
	if mutate != nil {
		mutate(claims)
	}
	token := jwt.NewWithClaims(jwt.SigningMethodEdDSA, claims)
	if keyID != "" {
		token.Header["kid"] = keyID
	}
	signed, err := token.SignedString(private)
	if err != nil {
		t.Fatalf("signing test token: %v", err)
	}
	return signed
}

func newKey(t *testing.T) (ed25519.PublicKey, ed25519.PrivateKey) {
	t.Helper()
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating key: %v", err)
	}
	return public, private
}

func TestVerifyAcceptsAWellFormedToken(t *testing.T) {
	public, private := newKey(t)
	verifier := NewVerifier(StaticKeys{"k1": public}, testIssuer)

	identity, err := verifier.Verify(context.Background(), signedToken(t, private, "k1", nil))
	if err != nil {
		t.Fatalf("Verify() error = %v", err)
	}
	if identity.Audience != AudienceParticipant {
		t.Errorf("audience = %q, want participant", identity.Audience)
	}
	if identity.AccountID != "44a1b7c2-0d3e-4f51-9a86-2c7d1e5f0a93" {
		t.Errorf("account = %q", identity.AccountID)
	}
	if identity.SessionID != "9b2f4d61-8c3a-4e57-b0d1-6f8a2c4e7d95" {
		t.Errorf("session = %q", identity.SessionID)
	}
	if len(identity.Scopes) != 1 || identity.Scopes[0] != "geodata.review" {
		t.Errorf("scopes = %v", identity.Scopes)
	}
}

func TestVerifyRejects(t *testing.T) {
	public, private := newKey(t)
	_, otherPrivate := newKey(t)

	cases := []struct {
		name   string
		token  func() string
		reason string
	}{
		{
			name:   "a token signed by another key",
			token:  func() string { return signedToken(t, otherPrivate, "k1", nil) },
			reason: "forged signature",
		},
		{
			name: "an expired token",
			token: func() string {
				return signedToken(t, private, "k1", func(c jwt.MapClaims) { c["exp"] = time.Now().Add(-time.Hour).Unix() })
			},
			reason: "past its expiry",
		},
		{
			name:   "a token with no expiry",
			token:  func() string { return signedToken(t, private, "k1", func(c jwt.MapClaims) { delete(c, "exp") }) },
			reason: "no expiry to enforce",
		},
		{
			name: "a token from another issuer",
			token: func() string {
				return signedToken(t, private, "k1", func(c jwt.MapClaims) { c["iss"] = "somebody-else" })
			},
			reason: "wrong issuer",
		},
		{
			name:   "a token with an unknown audience",
			token:  func() string { return signedToken(t, private, "k1", func(c jwt.MapClaims) { c["aud"] = "internal" }) },
			reason: "unknown audience",
		},
		{
			name: "a token claiming both audiences",
			token: func() string {
				return signedToken(t, private, "k1", func(c jwt.MapClaims) {
					c["aud"] = []string{string(AudienceParticipant), string(AudienceAdmin)}
				})
			},
			reason: "ambiguous audience",
		},
		{
			name:   "a token with no subject",
			token:  func() string { return signedToken(t, private, "k1", func(c jwt.MapClaims) { delete(c, "sub") }) },
			reason: "no account",
		},
		{
			name:   "a token with no session id",
			token:  func() string { return signedToken(t, private, "k1", func(c jwt.MapClaims) { delete(c, "sid") }) },
			reason: "not revocable",
		},
		{
			// The classic forgery: `alg: none` with no signature at all.
			name: "an unsigned token",
			token: func() string {
				token := jwt.NewWithClaims(jwt.SigningMethodNone, jwt.MapClaims{
					"iss": testIssuer, "aud": string(AudienceAdmin), "sub": "x", "sid": "y",
					"exp": time.Now().Add(time.Hour).Unix(),
				})
				signed, err := token.SignedString(jwt.UnsafeAllowNoneSignatureType)
				if err != nil {
					t.Fatalf("building an unsigned token: %v", err)
				}
				return signed
			},
			reason: "no signature",
		},
		{
			// The other classic: an asymmetric public key reinterpreted as an HMAC
			// secret, so the attacker can sign with material everyone has.
			name: "an HS256 token signed with the public key",
			token: func() string {
				token := jwt.NewWithClaims(jwt.SigningMethodHS256, jwt.MapClaims{
					"iss": testIssuer, "aud": string(AudienceAdmin), "sub": "x", "sid": "y",
					"exp": time.Now().Add(time.Hour).Unix(),
				})
				signed, err := token.SignedString([]byte(public))
				if err != nil {
					t.Fatalf("signing with the public key: %v", err)
				}
				return signed
			},
			reason: "algorithm confusion",
		},
		{
			name:   "a token with an unknown key id",
			token:  func() string { return signedToken(t, private, "unknown-kid", nil) },
			reason: "no key to check it against",
		},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			verifier := NewVerifier(StaticKeys{"k1": public}, testIssuer)
			_, err := verifier.Verify(context.Background(), c.token())
			if err == nil {
				t.Fatalf("Verify() accepted a token that should have been refused (%s)", c.reason)
			}
			if !errors.Is(err, ErrInvalidToken) && !errors.Is(err, ErrNoKey) {
				t.Errorf("error = %v, want ErrInvalidToken or ErrNoKey", err)
			}
		})
	}
}

func TestVerifyRefusesEverythingWithoutAKey(t *testing.T) {
	_, private := newKey(t)
	verifier := NewVerifier(StaticKeys{}, testIssuer)

	_, err := verifier.Verify(context.Background(), signedToken(t, private, "k1", nil))
	if !errors.Is(err, ErrNoKey) {
		t.Fatalf("error = %v, want ErrNoKey", err)
	}
	if verifier.Configured() {
		t.Error("Configured() = true with no keys, want false")
	}
}

func TestVerifyRefusesEverythingWithoutAnIssuer(t *testing.T) {
	public, private := newKey(t)
	verifier := NewVerifier(StaticKeys{"k1": public}, "")

	if verifier.Configured() {
		t.Error("Configured() = true with no issuer, want false")
	}
	if _, err := verifier.Verify(context.Background(), signedToken(t, private, "k1", nil)); err == nil {
		t.Error("Verify() accepted a token with no issuer configured")
	}
}

func TestVerifyAcceptsANewKeyAfterARotation(t *testing.T) {
	public, private := newKey(t)
	newPublic, newPrivate := newKey(t)

	// A source that learns the second key only when asked for it, like a JWKS after
	// `core` rotates.
	source := &rotatingKeys{keys: StaticKeys{"old": public}, later: StaticKeys{"new": newPublic}}
	verifier := NewVerifier(source, testIssuer)

	if _, err := verifier.Verify(context.Background(), signedToken(t, private, "old", nil)); err != nil {
		t.Fatalf("old key rejected: %v", err)
	}
	if _, err := verifier.Verify(context.Background(), signedToken(t, newPrivate, "new", nil)); err != nil {
		t.Fatalf("rotated key rejected: %v", err)
	}
}

// rotatingKeys answers from `keys` first and from `later` afterwards, so a test can
// assert the verifier does not cache the absence of a key.
type rotatingKeys struct {
	keys  StaticKeys
	later StaticKeys
}

func (r *rotatingKeys) Key(ctx context.Context, keyID string) (ed25519.PublicKey, error) {
	if key, err := r.keys.Key(ctx, keyID); err == nil {
		return key, nil
	}
	return r.later.Key(ctx, keyID)
}
