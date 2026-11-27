// Package requestid generates and propagates the request identifier.
//
// Every response carries `X-Request-ID` and every problem detail repeats it, so a
// user report ("I got an error at 10:04") can be tied to one request in the logs.
// Generated at the edge rather than by each service, because a service that
// generates its own id produces an id nothing upstream can correlate.
package requestid

import (
	"context"
	"crypto/rand"
	"fmt"
	"regexp"
)

// Header is the request and response header name.
const Header = "X-Request-ID"

// An incoming id is trusted only if it cannot forge a log line or a JSON value:
// UUID-shaped, nothing else. Accepting an arbitrary string means a caller can put
// newlines into the log, which is log injection rather than a feature.
var wellFormed = regexp.MustCompile(`^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`)

type contextKey struct{}

// FromHeader returns the caller's id when it is well formed, and a fresh one
// otherwise. A caller-supplied id is preserved so a client can correlate its own
// retries; an id that does not look like a UUID is replaced rather than sanitised,
// because a truncated id is still an untrusted string in the logs.
func FromHeader(value string) string {
	if wellFormed.MatchString(value) {
		return value
	}
	return Generate()
}

// Generate returns a random version 4 UUID.
func Generate() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		// crypto/rand failing is not a recoverable condition in a request path, and
		// returning a zero id would silently collapse every request into one log
		// line. Panicking is caught by the recovery middleware and reported.
		panic(fmt.Sprintf("requestid: reading random bytes: %v", err))
	}
	b[6] = (b[6] & 0x0f) | 0x40 // version 4
	b[8] = (b[8] & 0x3f) | 0x80 // variant RFC 4122
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

// NewContext stores the id in a context.
func NewContext(ctx context.Context, id string) context.Context {
	return context.WithValue(ctx, contextKey{}, id)
}

// FromContext returns the id, or the empty string when there is none. An empty
// value is honest: a fabricated placeholder would be indistinguishable from a real
// id in a log search.
func FromContext(ctx context.Context) string {
	id, _ := ctx.Value(contextKey{}).(string)
	return id
}
