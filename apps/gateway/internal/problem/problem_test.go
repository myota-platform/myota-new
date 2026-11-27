package problem

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

func TestWriteStatusUsesTheClassStatusAndContentType(t *testing.T) {
	cases := []struct {
		class  Type
		status int
	}{
		{TypeValidation, http.StatusBadRequest},
		{TypeUnauthorized, http.StatusUnauthorized},
		{TypeWrongAudience, http.StatusUnauthorized},
		{TypeNotFound, http.StatusNotFound},
		{TypeUpstreamUnavailable, http.StatusServiceUnavailable},
		{TypeInternal, http.StatusInternalServerError},
	}
	for _, c := range cases {
		t.Run(string(c.class), func(t *testing.T) {
			recorder := httptest.NewRecorder()
			request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
			WriteStatus(recorder, request, c.class, "detail here")

			if recorder.Code != c.status {
				t.Errorf("status = %d, want %d", recorder.Code, c.status)
			}
			if got := recorder.Header().Get("Content-Type"); got != "application/problem+json" {
				t.Errorf("Content-Type = %q", got)
			}
			var detail Detail
			if err := json.Unmarshal(recorder.Body.Bytes(), &detail); err != nil {
				t.Fatalf("body is not JSON: %s", recorder.Body.String())
			}
			if detail.Type != c.class {
				t.Errorf("type = %q, want %q", detail.Type, c.class)
			}
			if detail.Status != c.status {
				t.Errorf("body status = %d, want the same as the HTTP status", detail.Status)
			}
			if detail.Title == "" {
				t.Error("no title")
			}
			if detail.Instance != "/v1/me" {
				t.Errorf("instance = %q, want the request path", detail.Instance)
			}
		})
	}
}

func TestWriteCarriesTheRequestID(t *testing.T) {
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
	request = request.WithContext(requestid.NewContext(request.Context(), "9f2c1e00-0000-4000-8000-000000000001"))

	WriteStatus(recorder, request, TypeWrongAudience, "nope")

	var detail Detail
	body, _ := io.ReadAll(recorder.Body)
	if err := json.Unmarshal(body, &detail); err != nil {
		t.Fatalf("body is not JSON: %s", body)
	}
	if detail.RequestID != "9f2c1e00-0000-4000-8000-000000000001" {
		t.Errorf("requestId = %q, want the context's id", detail.RequestID)
	}
}

func TestMarshalMatchesWrite(t *testing.T) {
	// Marshal exists for the reverse proxy's response rewriter. If the two encoders
	// ever diverged, one path would serve a shape the other does not.
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/v1/me", nil)
	request = request.WithContext(requestid.NewContext(request.Context(), "9f2c1e00-0000-4000-8000-000000000001"))
	Write(recorder, request, TypeNotFound, http.StatusNotFound, "nothing here")

	fromMarshal, err := Marshal(TypeNotFound, http.StatusNotFound, "nothing here", "/v1/me",
		"9f2c1e00-0000-4000-8000-000000000001")
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}

	var written, marshalled Detail
	if err := json.Unmarshal(recorder.Body.Bytes(), &written); err != nil {
		t.Fatalf("decoding Write output: %v", err)
	}
	if err := json.Unmarshal(fromMarshal, &marshalled); err != nil {
		t.Fatalf("decoding Marshal output: %v", err)
	}
	if written != marshalled {
		t.Errorf("Write produced %+v, Marshal produced %+v", written, marshalled)
	}
}

func TestDetailOmitsEmptyFields(t *testing.T) {
	// `detail` and `instance` are omitted when empty rather than sent as null, so a
	// generated client's optional field stays optional.
	encoded, err := Marshal(TypeInternal, http.StatusInternalServerError, "", "", "")
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	var fields map[string]any
	if err := json.Unmarshal(encoded, &fields); err != nil {
		t.Fatalf("decoding: %v", err)
	}
	for _, absent := range []string{"detail", "instance", "requestId"} {
		if _, present := fields[absent]; present {
			t.Errorf("%q is present in an otherwise empty problem detail", absent)
		}
	}
	for _, required := range []string{"type", "title", "status"} {
		if _, present := fields[required]; !present {
			t.Errorf("%q is missing, and RFC 9457 requires it", required)
		}
	}
}
