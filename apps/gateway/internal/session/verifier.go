// Package session verifies the access tokens the gateway forwards.
//
// Two audiences, one identity store (`myota_core`), no shared session. The token
// carries the audience in its `aud` claim, and the gateway's only authorisation
// decision is whether that audience may use the route class it asked for. Scopes,
// ownership and every domain rule stay in the services.
//
// ## Why the key comes from `core` rather than from the gateway's config
//
// `core` signs with an Ed25519 private key it holds alone; the gateway only needs
// the public half. Shipping a copy of the public key in the gateway's configuration
// would freeze it at deploy time, so the 180-day rollover documented in
// `secrets-and-config.md` would mean redeploying the gateway to accept new tokens —
// and during the overlap period it would reject half of them. Instead `core`
// publishes a JWKS at `/internal/auth/jwks`, the gateway fetches it, caches by
// `kid`, and refetches when it meets a `kid` it does not know. See ADR-0008.
//
// ## Fail closed
//
// With no usable key the verifier reports ErrNoKey. The caller turns that into a 401
// on the routes it guards. It deliberately does not fall back to parsing the token
// without verifying it: an unverified `aud` claim is a claim anyone can write, and
// "the gateway trusts an unsigned token" is a worse failure than "the gateway
// refuses everything until its key source is configured".
package session

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/golang-jwt/jwt/v5"
)

// Audience is the value of the token's `aud` claim.
type Audience string

// The two audiences. A token carries exactly one; a session cannot be both.
const (
	AudienceParticipant Audience = "participant"
	AudienceAdmin       Audience = "admin"
)

// Identity is what the gateway needs from a verified token. Nothing else is
// exposed, because every additional field is a field some caller will start
// trusting for authorisation.
type Identity struct {
	AccountID string
	SessionID string
	Audience  Audience
	Scopes    []string
	ExpiresAt time.Time
}

// Errors a caller has to distinguish: an absent key source is a configuration
// problem the operator must fix, while an invalid token is the caller's problem.
var (
	// ErrNoKey means the verifier has no key material at all.
	ErrNoKey = errors.New("session: no verification key available")
	// ErrInvalidToken means the token failed parsing, signature or claim checks.
	ErrInvalidToken = errors.New("session: invalid token")
)

// claims is the token shape, and only the claims the gateway acts on. jwt's
// RegisteredClaims supplies `exp`, `iat`, `aud` and `iss` handling.
type claims struct {
	SessionID string   `json:"sid"`
	Scopes    []string `json:"scp"`
	jwt.RegisteredClaims
}

// Verifier checks tokens against one or more keys.
type Verifier struct {
	keys KeySource
	// issuer is required: a token signed by a key of ours but issued by something
	// else is not a Myota session, and accepting it would make key custody the only
	// thing standing between a stray signer and the admin console.
	issuer string
	// now is injectable so expiry can be tested without sleeping.
	now func() time.Time
	// leeway tolerates small clock differences between core and the gateway. Ten
	// seconds, not minutes: a generous window is a window in which a revoked token
	// still works.
	leeway time.Duration
}

// NewVerifier builds a verifier. An empty issuer is accepted so the gateway can
// start without configuration and report honestly through /readyz, but Verify will
// refuse every token in that state rather than skip the issuer check.
func NewVerifier(keys KeySource, issuer string) *Verifier {
	return &Verifier{keys: keys, issuer: issuer, now: time.Now, leeway: 10 * time.Second}
}

// Verify checks a bearer token and returns the identity it asserts.
func (v *Verifier) Verify(ctx context.Context, tokenString string) (Identity, error) {
	var identity Identity

	// Guarded here rather than left to jwt.WithIssuer. jwt/v5 treats an empty
	// expected issuer as "do not check", so passing "" would silently accept a token
	// issued by anyone whose key we happen to trust. A test caught exactly that;
	// the check stays because a library's default is not a policy.
	if v.issuer == "" {
		return identity, fmt.Errorf("%w: no issuer configured", ErrNoKey)
	}

	parsed, err := jwt.ParseWithClaims(tokenString, &claims{}, func(token *jwt.Token) (any, error) {
		kid, _ := token.Header["kid"].(string)
		key, keyErr := v.keys.Key(ctx, kid)
		if keyErr != nil {
			return nil, keyErr
		}
		return key, nil
	},
		// Pinning the algorithm is not optional. Without it, a caller can present a
		// token signed `alg: none` or with an asymmetric algorithm reinterpreting our
		// public key as an HMAC secret — the two classic JWT forgeries.
		jwt.WithValidMethods([]string{"EdDSA"}),
		jwt.WithExpirationRequired(),
		jwt.WithIssuer(v.issuer),
		jwt.WithAudience(string(AudienceAdmin), string(AudienceParticipant)),
		jwt.WithTimeFunc(v.now),
		jwt.WithLeeway(v.leeway),
	)
	if err != nil {
		if errors.Is(err, ErrNoKey) {
			return identity, err
		}
		return identity, fmt.Errorf("%w: %w", ErrInvalidToken, err)
	}

	parsedClaims, ok := parsed.Claims.(*claims)
	if !ok || !parsed.Valid {
		return identity, fmt.Errorf("%w: claims are not a Myota session", ErrInvalidToken)
	}

	// aud may carry both values; the first recognised one decides. A token claiming
	// both audiences is a signer bug, and treating it as the more privileged value
	// would be the wrong way to resolve it — so it is refused rather than resolved.
	audiences := parsedClaims.Audience
	if len(audiences) != 1 {
		return identity, fmt.Errorf("%w: expected exactly one audience, got %d", ErrInvalidToken, len(audiences))
	}
	switch Audience(audiences[0]) {
	case AudienceAdmin, AudienceParticipant:
		identity.Audience = Audience(audiences[0])
	default:
		return identity, fmt.Errorf("%w: unknown audience %q", ErrInvalidToken, audiences[0])
	}

	if parsedClaims.Subject == "" {
		return identity, fmt.Errorf("%w: no subject", ErrInvalidToken)
	}
	if parsedClaims.SessionID == "" {
		return identity, fmt.Errorf("%w: no session id", ErrInvalidToken)
	}

	identity.AccountID = parsedClaims.Subject
	identity.SessionID = parsedClaims.SessionID
	identity.Scopes = parsedClaims.Scopes
	if parsedClaims.ExpiresAt != nil {
		identity.ExpiresAt = parsedClaims.ExpiresAt.Time
	}
	return identity, nil
}

// Issuer returns the issuer this verifier requires. Used by /readyz to report
// whether the gateway is configured to verify anything.
func (v *Verifier) Issuer() string {
	return v.issuer
}

// Configured reports whether the verifier has the two things it needs to accept a
// token. /readyz and the admin-route middleware both read it, so "not configured"
// has one definition.
func (v *Verifier) Configured() bool {
	if v.issuer == "" {
		return false
	}
	_, err := v.keys.Key(context.Background(), "")
	return err == nil || !errors.Is(err, ErrNoKey)
}
