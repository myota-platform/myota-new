// Package problem writes RFC 9457 problem details.
//
// One error shape for the whole edge. The gateway, every service and every client
// speak `application/problem+json`; a second envelope would mean every caller
// carrying two error parsers, and the one nobody tests is the one that breaks.
//
//	{
//	  "type":   "about:blank#wrong-audience",
//	  "title":  "Wrong audience for this route",
//	  "status": 401,
//	  "detail": "…",
//	  "requestId": "9f2c…"
//	}
//
// `type` is a fragment identifier relative to `about:blank` rather than a URL, so a
// client can branch on it without a network round trip, and adding a new class
// needs no hosting. The values here are the same set the contract declares in
// contracts/shared/components.yaml — a `type` the contract does not list would be
// an error class no generated client knows how to handle.
package problem

import (
	"encoding/json"
	"log/slog"
	"net/http"

	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

// Type identifies a problem class. It is the value of the `type` member.
type Type string

// The problem classes the gateway itself produces. Services produce more; these
// are the ones the edge can emit before a request reaches an upstream.
const (
	// TypeValidation means the request was malformed at the edge — a body over the
	// limit, an unparseable header.
	TypeValidation Type = "about:blank#validation-failed"
	// TypeUnauthorized means no credential, or one that failed verification.
	TypeUnauthorized Type = "about:blank#unauthorized"
	// TypeWrongAudience means a valid credential of the wrong audience reached a
	// route class that does not accept it. Separate from TypeUnauthorized because
	// the fix is different: the caller is signed in, just not as the right kind of
	// account.
	TypeWrongAudience Type = "about:blank#wrong-audience"
	// TypeNotFound means no route matched. Never a guessed upstream.
	TypeNotFound Type = "about:blank#not-found"
	// TypeUpstreamUnavailable means an upstream refused the connection or timed out.
	TypeUpstreamUnavailable Type = "about:blank#upstream-unavailable"
	// TypeInternal means the gateway itself failed. Should be unreachable.
	TypeInternal Type = "about:blank#internal"
)

// Detail is one problem occurrence.
type Detail struct {
	Type      Type   `json:"type"`
	Title     string `json:"title"`
	Status    int    `json:"status"`
	Detail    string `json:"detail,omitempty"`
	Instance  string `json:"instance,omitempty"`
	RequestID string `json:"requestId,omitempty"`
}

// titles is the stable human-readable summary per class. It is a map rather than a
// field on every call site so the same `type` cannot be shown under two titles —
// which is the sort of inconsistency that makes a client's error handling a
// guessing game.
var titles = map[Type]string{
	TypeValidation:          "Request validation failed",
	TypeUnauthorized:        "Not authenticated",
	TypeWrongAudience:       "Wrong audience for this route",
	TypeNotFound:            "Not found",
	TypeUpstreamUnavailable: "Upstream unavailable",
	TypeInternal:            "Internal error",
}

// Marshal returns the encoded problem detail. It exists separately from Write so a
// component that is not an http.ResponseWriter — the reverse proxy's response
// rewriter — can produce the identical body. Two encoders would be two formats
// within a month.
func Marshal(class Type, status int, detail, instance, requestID string) ([]byte, error) {
	return json.Marshal(Detail{
		Type:      class,
		Title:     titles[class],
		Status:    status,
		Detail:    detail,
		Instance:  instance,
		RequestID: requestID,
	})
}

// Write sends a problem detail response. A failure to encode is logged and the
// connection closed rather than compounded: there is nothing useful to send after
// the status line is already written.
func Write(w http.ResponseWriter, r *http.Request, class Type, status int, detail string) {
	body, err := Marshal(class, status, detail, r.URL.Path, requestid.FromContext(r.Context()))
	if err != nil {
		slog.ErrorContext(r.Context(), "encoding problem response", "error", err)
		w.WriteHeader(http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/problem+json")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(status)
	if _, err := w.Write(body); err != nil {
		slog.ErrorContext(r.Context(), "writing problem response", "error", err)
	}
}

// WriteStatus is Write for the classes with a fixed status code, so a caller cannot
// pair `wrong-audience` with a 500 by accident.
func WriteStatus(w http.ResponseWriter, r *http.Request, class Type, detail string) {
	Write(w, r, class, statusFor(class), detail)
}

// statusFor is the one place a problem class maps to a status code.
func statusFor(class Type) int {
	switch class {
	case TypeValidation:
		return http.StatusBadRequest
	case TypeUnauthorized, TypeWrongAudience:
		return http.StatusUnauthorized
	case TypeNotFound:
		return http.StatusNotFound
	case TypeUpstreamUnavailable:
		return http.StatusServiceUnavailable
	default:
		return http.StatusInternalServerError
	}
}
