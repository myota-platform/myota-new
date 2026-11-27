package edge

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

func TestRequestIDPreservesAWellFormedID(t *testing.T) {
	const supplied = "9f2c1e00-0000-4000-8000-000000000001"
	var seenInContext string
	handler := RequestID(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		seenInContext = requestid.FromContext(r.Context())
	}))

	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
	request.Header.Set(requestid.Header, supplied)
	handler.ServeHTTP(recorder, request)

	if seenInContext != supplied {
		t.Errorf("context id = %q, want %q", seenInContext, supplied)
	}
	if got := recorder.Header().Get(requestid.Header); got != supplied {
		t.Errorf("response header = %q, want %q", got, supplied)
	}
}

func TestRequestIDReplacesAnUntrustedID(t *testing.T) {
	var seenInContext string
	handler := RequestID(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		seenInContext = requestid.FromContext(r.Context())
	}))

	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
	request.Header.Set(requestid.Header, "not-a-uuid")
	handler.ServeHTTP(recorder, request)

	if seenInContext == "not-a-uuid" || seenInContext == "" {
		t.Errorf("context id = %q, want a generated replacement", seenInContext)
	}
	if got := recorder.Header().Get(requestid.Header); got != seenInContext {
		t.Errorf("response header %q does not match the context id %q", got, seenInContext)
	}
}

func TestTraceParent(t *testing.T) {
	const supplied = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
	const requestID = "9f2c1e00-0000-4000-8000-000000000001"

	cases := []struct {
		name   string
		header string
		check  func(t *testing.T, got string)
	}{
		{
			name:   "a valid trace context is forwarded unchanged",
			header: supplied,
			check: func(t *testing.T, got string) {
				if got != supplied {
					t.Errorf("traceparent = %q, want it forwarded unchanged", got)
				}
			},
		},
		{
			name:   "an absent trace context is created from the request id",
			header: "",
			check: func(t *testing.T, got string) {
				if !traceparentShape.MatchString(got) {
					t.Fatalf("traceparent = %q, want a well-formed one", got)
				}
				if !strings.Contains(got, "9f2c1e00000040008000000000000001") {
					t.Errorf("traceparent = %q, want the trace id derived from the request id", got)
				}
			},
		},
		{
			name:   "a malformed trace context is replaced, not rejected",
			header: "00-not-hex-00",
			check: func(t *testing.T, got string) {
				if !traceparentShape.MatchString(got) {
					t.Errorf("traceparent = %q, want a well-formed replacement", got)
				}
			},
		},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var seen string
			handler := TraceParent(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
				seen = r.Header.Get(traceparentHeader)
			}))

			request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
			request = request.WithContext(requestid.NewContext(request.Context(), requestID))
			if c.header != "" {
				request.Header.Set(traceparentHeader, c.header)
			}
			handler.ServeHTTP(httptest.NewRecorder(), request)

			c.check(t, seen)
		})
	}
}

func TestBodyLimitRejectsADeclaredOversizeBody(t *testing.T) {
	reached := false
	handler := BodyLimit(1024)(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		reached = true
	}))

	request := httptest.NewRequest(http.MethodPost, "/v1/entities", strings.NewReader(strings.Repeat("x", 2048)))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, request)

	if reached {
		t.Error("handler ran despite an oversized body")
	}
	if recorder.Code != http.StatusBadRequest {
		t.Errorf("status = %d, want %d", recorder.Code, http.StatusBadRequest)
	}
	if got := recorder.Header().Get("Content-Type"); got != "application/problem+json" {
		t.Errorf("Content-Type = %q, want application/problem+json", got)
	}
}

func TestBodyLimitCapsABodyThatLiesAboutItsLength(t *testing.T) {
	// A chunked body reports ContentLength -1, so only the reader cap can stop it.
	var readErr error
	handler := BodyLimit(16)(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		buffer := make([]byte, 64)
		_, readErr = r.Body.Read(buffer)
	}))

	request := httptest.NewRequest(http.MethodPost, "/v1/entities", strings.NewReader(strings.Repeat("x", 128)))
	request.ContentLength = -1
	handler.ServeHTTP(httptest.NewRecorder(), request)

	if readErr == nil {
		t.Fatal("reading past the cap returned no error")
	}
	if !strings.Contains(readErr.Error(), "request body too large") {
		t.Errorf("read error = %v, want the MaxBytesReader error", readErr)
	}
}

func TestRecoverTurnsAPanicIntoA500(t *testing.T) {
	handler := RequestID(Recover(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		panic("boom")
	})))

	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/v1/me", nil))

	if recorder.Code != http.StatusInternalServerError {
		t.Errorf("status = %d, want 500", recorder.Code)
	}
	if got := recorder.Header().Get("Content-Type"); got != "application/problem+json" {
		t.Errorf("Content-Type = %q, want application/problem+json", got)
	}
	body := recorder.Body.String()
	if strings.Contains(body, "boom") || strings.Contains(body, "goroutine") {
		t.Errorf("response leaked the panic: %s", body)
	}
	if !strings.Contains(body, "9f2c") && !strings.Contains(body, "requestId") {
		t.Errorf("response has no requestId to trace: %s", body)
	}
}
