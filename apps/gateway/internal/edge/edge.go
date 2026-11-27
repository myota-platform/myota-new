// Package edge holds the middleware the gateway applies before anything is
// proxied: identity of the request, trace context, body size, and a last-resort
// panic handler.
//
// Everything here is stateless. No middleware counts, caches or remembers anything
// across requests, because that is what lets the gateway run as N interchangeable
// replicas (ADR-0005). Rate limiting in particular is absent by design: a counter
// in a replica multiplies the configured limit by the replica count, so it lives in
// Traefik at the single ingress.
package edge

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"log/slog"
	"net/http"
	"regexp"
	"runtime/debug"
	"strings"

	"github.com/myota-platform/myota-new/apps/gateway/internal/problem"
	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

// MaxJSONBodyBytes is the largest request body the gateway accepts. It matches the
// documented 2 MiB limit; uploads never come through here because they are
// presigned to object storage, so a body over this is a mistake or an attack rather
// than a large file.
const MaxJSONBodyBytes int64 = 2 << 20

// RequestID keeps or generates `X-Request-ID`, puts it in the context and echoes it
// on the response. It runs first so every later middleware, including the recovery
// handler, can name the request it failed on.
func RequestID(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		id := requestid.FromHeader(r.Header.Get(requestid.Header))
		// Set before next.ServeHTTP so the response carries the id even when the
		// handler writes its own status — a header set afterwards is already sent.
		w.Header().Set(requestid.Header, id)
		next.ServeHTTP(w, r.WithContext(requestid.NewContext(r.Context(), id)))
	})
}

// traceparentHeader is the W3C Trace Context header. The format is
// `version-traceid-parentid-flags`, all lowercase hex.
const traceparentHeader = "traceparent"

var traceparentShape = regexp.MustCompile(`^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$`)

// TraceParent forwards a valid inbound trace context and creates a root one when it
// is absent or malformed.
//
// A malformed header is replaced rather than rejected: a broken trace header is a
// client bug, and failing the request over it would turn an observability detail
// into an outage. The replacement is derived from the request id so the trace and
// the log line are the same identifier.
func TraceParent(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !traceparentShape.MatchString(r.Header.Get(traceparentHeader)) {
			r.Header.Set(traceparentHeader, newTraceParent(requestid.FromContext(r.Context())))
		}
		next.ServeHTTP(w, r)
	})
}

// newTraceParent builds a root trace context. The trace id is the request id with
// its dashes removed, so a trace and the request that started it share one value;
// the parent id is random per hop.
func newTraceParent(requestID string) string {
	traceID := strings.ReplaceAll(requestID, "-", "")
	if len(traceID) != 32 {
		// No request id in context means RequestID did not run. A random trace id is
		// still better than a malformed one, and cannot collide.
		traceID = randomHex(16)
	}
	return fmt.Sprintf("00-%s-%s-01", traceID, randomHex(8))
}

// randomHex returns n random bytes as 2n lowercase hex characters.
func randomHex(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic(fmt.Sprintf("edge: reading random bytes: %v", err))
	}
	return hex.EncodeToString(b)
}

// BodyLimit rejects a request whose declared body is larger than MaxJSONBodyBytes,
// and caps a body that lies about its length.
//
// The declared size is checked first so an oversized request is refused with a 413
// and a problem detail instead of being read to the limit. The reader is capped as
// well, because `Content-Length` is caller-supplied: a chunked body with no length
// at all would otherwise stream without bound. Exceeding the cap mid-read surfaces
// as *http.MaxBytesError, which the proxy turns into the same 413.
func BodyLimit(limit int64) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.ContentLength > limit {
				problem.WriteStatus(w, r, problem.TypeValidation,
					fmt.Sprintf("Request body is %d bytes; the limit is %d.", r.ContentLength, limit))
				return
			}
			r.Body = http.MaxBytesReader(w, r.Body, limit)
			next.ServeHTTP(w, r)
		})
	}
}

// Recover turns a panic into a 500 problem detail and keeps the process alive.
//
// A panic in one request must not take the replica's other in-flight requests with
// it: the gateway is a shared path for every client, and a nil dereference in a
// middleware would otherwise be a deployment-wide outage triggered by one malformed
// request. The stack is logged, not returned — a stack trace in a response is an
// information leak.
func Recover(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() {
			recovered := recover()
			if recovered == nil {
				return
			}
			// http.ErrAbortHandler is the documented way to abort without a response;
			// re-panicking lets net/http handle it rather than logging a false error.
			if recovered == http.ErrAbortHandler {
				panic(recovered)
			}
			slog.ErrorContext(r.Context(), "panic recovered",
				"panic", recovered,
				"path", r.URL.Path,
				"method", r.Method,
				"request_id", requestid.FromContext(r.Context()),
				"stack", string(debug.Stack()),
			)
			problem.WriteStatus(w, r, problem.TypeInternal, "The gateway failed to handle the request.")
		}()
		next.ServeHTTP(w, r)
	})
}
