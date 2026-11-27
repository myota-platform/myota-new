package upstream

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/myota-platform/myota-new/apps/gateway/internal/problem"
	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

// serve runs handler on a test server and returns a Service pointed at it.
func serve(t *testing.T, handler http.Handler, timeout time.Duration) *Service {
	t.Helper()
	server := httptest.NewServer(handler)
	t.Cleanup(server.Close)
	service, err := New("core", server.URL, timeout, nil)
	if err != nil {
		t.Fatalf("upstream.New: %v", err)
	}
	return service
}

func TestProxyForwardsPathAndHeaders(t *testing.T) {
	var gotPath, gotQuery, gotRequestID, gotForwardedFor, gotTrace string
	service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotQuery = r.URL.RawQuery
		gotRequestID = r.Header.Get(requestid.Header)
		gotForwardedFor = r.Header.Get("X-Forwarded-For")
		gotTrace = r.Header.Get("traceparent")
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true}`))
	}), time.Second)

	request := httptest.NewRequest(http.MethodGet, "/v1/me/callsigns?limit=5", nil)
	request = request.WithContext(requestid.NewContext(request.Context(), "9f2c1e00-0000-4000-8000-000000000001"))
	request.Header.Set("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
	request.RemoteAddr = "203.0.113.9:54321"

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, request)

	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", recorder.Code)
	}
	if gotPath != "/v1/me/callsigns" || gotQuery != "limit=5" {
		t.Errorf("upstream saw %s?%s, want /v1/me/callsigns?limit=5", gotPath, gotQuery)
	}
	if gotRequestID != "9f2c1e00-0000-4000-8000-000000000001" {
		t.Errorf("X-Request-ID = %q, want it forwarded", gotRequestID)
	}
	if gotForwardedFor == "" {
		t.Error("X-Forwarded-For is empty, so the service cannot see the client address")
	}
	if gotTrace == "" {
		t.Error("traceparent was not forwarded")
	}
	if body := recorder.Body.String(); body != `{"ok":true}` {
		t.Errorf("body = %q, want the upstream's bytes verbatim", body)
	}
}

func TestProxyBasePathIsPreserved(t *testing.T) {
	// A service deployed under a sub-path must still receive the full path, not a
	// path relative to its base. This is the difference between SetURL and a naive
	// host swap.
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/entities/1" {
			t.Errorf("upstream saw path %q, want /v1/entities/1", r.URL.Path)
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()

	service, err := New("geodata", server.URL, time.Second, nil)
	if err != nil {
		t.Fatalf("upstream.New: %v", err)
	}
	service.Handler().ServeHTTP(httptest.NewRecorder(),
		httptest.NewRequest(http.MethodGet, "/v1/entities/1", nil))
}

func TestProxyReportsAnUnreachableUpstreamAs503(t *testing.T) {
	// Bind a port, then close it, so the connection is refused rather than refused by
	// DNS resolution — the case a real outage actually produces.
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	address := listener.Addr().String()
	_ = listener.Close()

	service, err := New("activity", "http://"+address, time.Second, nil)
	if err != nil {
		t.Fatalf("upstream.New: %v", err)
	}

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/v1/qsos/1", nil))

	if recorder.Code != http.StatusServiceUnavailable {
		t.Errorf("status = %d, want 503", recorder.Code)
	}
	if detail := assertProblem(t, recorder, problem.TypeUpstreamUnavailable); !strings.Contains(detail.Detail, "activity") {
		t.Errorf("detail = %q, want it to name the upstream", detail.Detail)
	}
}

func TestProxyReportsATimeoutAs504(t *testing.T) {
	service := serve(t, http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		select {
		case <-time.After(2 * time.Second):
		case <-r.Context().Done():
		}
	}), 50*time.Millisecond)

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/v1/me", nil))

	if recorder.Code != http.StatusGatewayTimeout {
		t.Errorf("status = %d, want 504", recorder.Code)
	}
	assertProblem(t, recorder, problem.TypeUpstreamUnavailable)
}

func TestProxyNormalisesANonProblem5xx(t *testing.T) {
	service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		// What a Go panic page or a framework error looks like: HTML, status 500.
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte("<html><body>500 Internal Server Error</body></html>"))
	}), time.Second)

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/v1/me", nil))

	if recorder.Code != http.StatusInternalServerError {
		t.Errorf("status = %d, want 500 preserved", recorder.Code)
	}
	assertProblem(t, recorder, problem.TypeUpstreamUnavailable)
	if strings.Contains(recorder.Body.String(), "<html>") {
		t.Errorf("body still contains the upstream's HTML: %s", recorder.Body.String())
	}
}

func TestProxyPreservesAnUpstreamProblemDetail(t *testing.T) {
	// A service that already speaks problem+json must not have its detail replaced:
	// the field-level errors are the useful part.
	upstreamBody := `{"type":"about:blank#conflict","title":"Already closed","status":409,"detail":"nope"}`
	service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/problem+json")
		w.WriteHeader(http.StatusConflict)
		_, _ = w.Write([]byte(upstreamBody))
	}), time.Second)

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/v1/activations/1/close", nil))

	if recorder.Code != http.StatusConflict {
		t.Errorf("status = %d, want 409", recorder.Code)
	}
	if got := strings.TrimSpace(recorder.Body.String()); got != upstreamBody {
		t.Errorf("body = %q, want the upstream's verbatim", got)
	}
}

func TestProxyLeavesSuccessfulResponsesAlone(t *testing.T) {
	service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte("created"))
	}), time.Second)

	recorder := httptest.NewRecorder()
	service.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/v1/me", nil))

	if recorder.Code != http.StatusCreated {
		t.Errorf("status = %d, want 201 passed through", recorder.Code)
	}
	if recorder.Body.String() != "created" {
		t.Errorf("body = %q, want it untouched", recorder.Body.String())
	}
}

func TestNewRejectsAMalformedBaseURL(t *testing.T) {
	cases := []string{"", "core-api", "//host", "127.0.0.1:8080"}
	for _, candidate := range cases {
		if _, err := New("core", candidate, time.Second, nil); err == nil {
			t.Errorf("New(%q) accepted a base URL with no scheme or host", candidate)
		}
	}
}

func TestProbe(t *testing.T) {
	t.Run("healthy upstream", func(t *testing.T) {
		service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path != "/internal/healthz" {
				t.Errorf("probe hit %q, want /internal/healthz", r.URL.Path)
			}
			_, _ = w.Write([]byte(`{"status":"ok","service":"core"}`))
		}), time.Second)
		if err := service.Probe(context.Background()); err != nil {
			t.Errorf("Probe() = %v, want nil", err)
		}
	})

	t.Run("unhealthy upstream", func(t *testing.T) {
		service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusServiceUnavailable)
		}), time.Second)
		if err := service.Probe(context.Background()); err == nil {
			t.Error("Probe() = nil, want an error for a 503")
		}
	})

	t.Run("results are cached", func(t *testing.T) {
		// A kubelet polls /readyz every few seconds; each poll must not become three
		// health requests to three services.
		var calls int
		service := serve(t, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			calls++
			_, _ = w.Write([]byte(`{"status":"ok"}`))
		}), time.Second)

		for range 5 {
			_ = service.Probe(context.Background())
		}
		if calls != 1 {
			t.Errorf("probe ran %d times in 5 calls, want 1", calls)
		}
	})
}

func TestSetRequiresEveryConfiguredUpstream(t *testing.T) {
	set, err := NewSet(map[string]string{
		"core":    "http://core-api:8080",
		"geodata": "http://geodata-api:8080",
	}, time.Second, nil)
	if err != nil {
		t.Fatalf("NewSet: %v", err)
	}
	if _, ok := set.Get("activity"); ok {
		t.Error("Get(activity) found an upstream that was not configured")
	}
	if names := set.Names(); len(names) != 2 || names[0] != "core" || names[1] != "geodata" {
		t.Errorf("Names() = %v, want them sorted", names)
	}
	if _, err := NewSet(map[string]string{"core": "not-a-url"}, time.Second, nil); err == nil {
		t.Error("NewSet accepted a malformed base URL")
	}
}

// assertProblem checks the response is a problem detail of the expected class and
// returns it, so a caller can also assert on the detail text without reading the
// body twice.
func assertProblem(t *testing.T, recorder *httptest.ResponseRecorder, want problem.Type) problem.Detail {
	t.Helper()
	if got := recorder.Header().Get("Content-Type"); got != "application/problem+json" {
		t.Fatalf("Content-Type = %q, want application/problem+json", got)
	}
	var detail problem.Detail
	body, err := io.ReadAll(recorder.Body)
	if err != nil {
		t.Fatalf("reading body: %v", err)
	}
	if err := json.Unmarshal(body, &detail); err != nil {
		t.Fatalf("body is not a problem detail: %s", body)
	}
	if detail.Type != want {
		t.Errorf("type = %q, want %q", detail.Type, want)
	}
	if detail.Title == "" {
		t.Error("problem detail has no title")
	}
	return detail
}
