package gateway

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"

	"github.com/myota-platform/myota-new/apps/gateway/internal/assets"
	"github.com/myota-platform/myota-new/apps/gateway/internal/config"
	"github.com/myota-platform/myota-new/apps/gateway/internal/playground"
	"github.com/myota-platform/myota-new/apps/gateway/internal/problem"
	"github.com/myota-platform/myota-new/apps/gateway/internal/session"
	"github.com/myota-platform/myota-new/apps/gateway/internal/upstream"
)

const testIssuer = "myota-core"

// harness is a gateway wired to fake upstreams, so a test exercises the real router,
// the real middleware and the real proxy rather than calling handlers directly.
type harness struct {
	gateway    *Gateway
	server     *httptest.Server
	upstream   map[string]*fakeUpstream
	privateKey ed25519.PrivateKey
}

type fakeUpstream struct {
	server *httptest.Server
	// calls records the paths this upstream was asked for, in order.
	calls []string
	// status and body control the response.
	status int
	body   string
}

// newHarness builds a gateway whose three upstreams are httptest servers.
func newHarness(t *testing.T, configure func(*harnessConfig)) *harness {
	t.Helper()

	fakes := map[string]*fakeUpstream{}
	baseURLs := map[string]string{}
	for _, name := range []string{"core", "geodata", "activity"} {
		fake := &fakeUpstream{status: http.StatusOK, body: `{"service":"` + name + `"}`}
		fake.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path == "/internal/healthz" {
				_, _ = w.Write([]byte(`{"status":"ok"}`))
				return
			}
			fake.calls = append(fake.calls, r.URL.Path)
			if fake.status != http.StatusOK {
				w.WriteHeader(fake.status)
				_, _ = w.Write([]byte(fake.body))
				return
			}
			_, _ = w.Write([]byte(fake.body))
		}))
		t.Cleanup(fake.server.Close)
		fakes[name] = fake
		baseURLs[name] = fake.server.URL
	}

	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating a test key: %v", err)
	}

	configuration := config.Config{
		ListenAddress:  ":0",
		Upstreams:      baseURLs,
		RequestTimeout: 5 * time.Second,
		JWTIssuer:      testIssuer,
	}
	settings := &harnessConfig{config: &configuration, publicKey: publicKey, privateKey: privateKey}
	if configure != nil {
		configure(settings)
	}

	upstreams, err := upstream.NewSet(configuration.Upstreams, configuration.RequestTimeout, nil)
	if err != nil {
		t.Fatalf("upstream.NewSet: %v", err)
	}
	reference, err := playground.New(assets.FS)
	if err != nil {
		t.Fatalf("playground.New: %v", err)
	}
	verifier := session.NewVerifier(session.StaticKeys{"test-key": settings.publicKey}, settings.config.JWTIssuer)

	service := New(configuration, upstreams, verifier, reference, NewMetrics())
	server := httptest.NewServer(service.Handler())
	t.Cleanup(server.Close)

	return &harness{gateway: service, server: server, upstream: fakes, privateKey: privateKey}
}

// harnessConfig lets a test change one thing about the default wiring.
type harnessConfig struct {
	config     *config.Config
	publicKey  ed25519.PublicKey
	privateKey ed25519.PrivateKey
}

// get performs a request against the gateway.
func (h *harness) do(t *testing.T, method, path string, headers map[string]string) *http.Response {
	t.Helper()
	request, err := http.NewRequestWithContext(context.Background(), method, h.server.URL+path, nil)
	if err != nil {
		t.Fatalf("building the request: %v", err)
	}
	for name, value := range headers {
		request.Header.Set(name, value)
	}
	response, err := h.server.Client().Do(request)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	return response
}

// token signs a token the way `core` does.
func (h *harness) token(t *testing.T, audience session.Audience, mutate func(jwt.MapClaims)) string {
	t.Helper()
	private := h.keyForTest(t)
	now := time.Now()
	claims := jwt.MapClaims{
		"iss": testIssuer,
		"aud": string(audience),
		"sub": "44a1b7c2-0d3e-4f51-9a86-2c7d1e5f0a93",
		"sid": "9b2f4d61-8c3a-4e57-b0d1-6f8a2c4e7d95",
		"iat": now.Unix(),
		"exp": now.Add(15 * time.Minute).Unix(),
	}
	if mutate != nil {
		mutate(claims)
	}
	signed, err := jwt.NewWithClaims(jwt.SigningMethodEdDSA, claims).SignedString(private)
	if err != nil {
		t.Fatalf("signing a test token: %v", err)
	}
	return signed
}

// keyForTest returns the private key the harness signed its static key with.
func (h *harness) keyForTest(t *testing.T) ed25519.PrivateKey {
	t.Helper()
	if h.privateKey == nil {
		t.Fatal("the harness has no private key")
	}
	return h.privateKey
}

func TestUnknownPrefixIs404ProblemJSON(t *testing.T) {
	h := newHarness(t, nil)

	for _, path := range []string{"/v1/unknown", "/v2/entities/1", "/nope", "/v1/admin", "/"} {
		response := h.do(t, http.MethodGet, path, nil)
		if response.StatusCode != http.StatusNotFound {
			t.Errorf("GET %s = %d, want 404", path, response.StatusCode)
		}
		if got := response.Header.Get("Content-Type"); got != "application/problem+json" {
			t.Errorf("GET %s Content-Type = %q, want application/problem+json", path, got)
		}
		detail := decodeProblem(t, response)
		if detail.Type != problem.TypeNotFound {
			t.Errorf("GET %s type = %q, want %q", path, detail.Type, problem.TypeNotFound)
		}
		if detail.RequestID == "" {
			t.Errorf("GET %s has no requestId to trace", path)
		}
	}

	// The critical part of "never a guessed route": nothing was sent upstream.
	for name, fake := range h.upstream {
		if len(fake.calls) != 0 {
			t.Errorf("%s received %v for an unmatched path", name, fake.calls)
		}
	}
}

func TestParticipantTokenOnAdminRoutesIsWrongAudience(t *testing.T) {
	h := newHarness(t, nil)
	token := h.token(t, session.AudienceParticipant, nil)

	// Every administrator prefix, because the criterion is about the route class and
	// not about one endpoint.
	paths := []string{
		"/v1/admin/identity/participants",
		"/v1/admin/identity/admins",
		"/v1/admin/auth/refresh",
		"/v1/admin/notifications",
		"/v1/admin/proposals",
	}
	for _, path := range paths {
		response := h.do(t, http.MethodGet, path, map[string]string{"Authorization": "Bearer " + token})
		if response.StatusCode != http.StatusUnauthorized {
			t.Errorf("GET %s = %d, want 401", path, response.StatusCode)
		}
		detail := decodeProblem(t, response)
		if detail.Type != problem.TypeWrongAudience {
			t.Errorf("GET %s type = %q, want %q", path, detail.Type, problem.TypeWrongAudience)
		}
	}

	// And nothing reached core: the refusal happens at the edge.
	if calls := h.upstream["core"].calls; len(calls) != 0 {
		t.Errorf("core received %v despite the audience refusal", calls)
	}
}

func TestAdminTokenOnAdminRoutesIsProxied(t *testing.T) {
	h := newHarness(t, nil)
	token := h.token(t, session.AudienceAdmin, nil)

	response := h.do(t, http.MethodGet, "/v1/admin/identity/participants",
		map[string]string{"Authorization": "Bearer " + token})
	if response.StatusCode != http.StatusOK {
		t.Fatalf("GET /v1/admin/identity/participants = %d, want 200", response.StatusCode)
	}
	if calls := h.upstream["core"].calls; len(calls) != 1 || calls[0] != "/v1/admin/identity/participants" {
		t.Errorf("core received %v, want the one proxied request", calls)
	}
}

func TestParticipantTokenIsAcceptedOutsideAdminRoutes(t *testing.T) {
	// The audience check must not leak onto the participant surface: a participant
	// token reaches /v1/activations/, which is activity's.
	h := newHarness(t, nil)
	token := h.token(t, session.AudienceParticipant, nil)

	response := h.do(t, http.MethodGet, "/v1/activations",
		map[string]string{"Authorization": "Bearer " + token})
	if response.StatusCode != http.StatusOK {
		t.Fatalf("GET /v1/activations = %d, want 200", response.StatusCode)
	}
	if calls := h.upstream["activity"].calls; len(calls) != 1 {
		t.Errorf("activity received %v, want the one proxied request", calls)
	}
}

func TestAdminLoginIsReachableWithoutAToken(t *testing.T) {
	// A login form cannot require a login. The contract marks these two paths
	// token-free, and the generated table carries exactly those names.
	h := newHarness(t, nil)

	response := h.do(t, http.MethodPost, "/v1/admin/auth/login", nil)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("POST /v1/admin/auth/login = %d, want it proxied", response.StatusCode)
	}
	for _, path := range []string{"/v1/admin/auth/refresh", "/v1/admin/auth/reauth"} {
		response := h.do(t, http.MethodPost, path, nil)
		if response.StatusCode != http.StatusUnauthorized {
			t.Errorf("POST %s = %d, want 401 without a token", path, response.StatusCode)
		}
	}
}

func TestParticipantSessionCookieIsRefusedOnAdminRoutes(t *testing.T) {
	// The cookie names encode the audience, so a participant cookie on an
	// administrator route is a wrong-audience request even though the cookie itself
	// is an opaque refresh credential the gateway cannot verify.
	h := newHarness(t, nil)

	response := h.do(t, http.MethodGet, "/v1/admin/identity/participants", map[string]string{
		"Cookie": participantSessionCookie + "=opaque",
	})
	if response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want 401", response.StatusCode)
	}
	if detail := decodeProblem(t, response); detail.Type != problem.TypeWrongAudience {
		t.Errorf("type = %q, want %q", detail.Type, problem.TypeWrongAudience)
	}
	if calls := h.upstream["core"].calls; len(calls) != 0 {
		t.Errorf("core received %v despite the cookie refusal", calls)
	}
}

func TestAdminSessionCookieIsForwarded(t *testing.T) {
	// The gateway cannot verify an opaque session cookie; `core` can, and does. The
	// gateway forwards rather than pretending to check.
	h := newHarness(t, nil)

	response := h.do(t, http.MethodGet, "/v1/admin/identity/participants", map[string]string{
		"Cookie": adminSessionCookie + "=opaque",
	})
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want the request forwarded to core", response.StatusCode)
	}
}

func TestInvalidTokenOnAnAdminRouteIs401Unauthorized(t *testing.T) {
	h := newHarness(t, nil)

	cases := map[string]string{
		"garbage":          "not-a-token",
		"wrong issuer":     h.token(t, session.AudienceAdmin, func(c jwt.MapClaims) { c["iss"] = "elsewhere" }),
		"expired":          h.token(t, session.AudienceAdmin, func(c jwt.MapClaims) { c["exp"] = time.Now().Add(-time.Hour).Unix() }),
		"unknown audience": h.token(t, session.AudienceAdmin, func(c jwt.MapClaims) { c["aud"] = "internal" }),
		"no session id":    h.token(t, session.AudienceAdmin, func(c jwt.MapClaims) { delete(c, "sid") }),
	}
	for name, token := range cases {
		t.Run(name, func(t *testing.T) {
			response := h.do(t, http.MethodGet, "/v1/admin/identity/participants",
				map[string]string{"Authorization": "Bearer " + token})
			if response.StatusCode != http.StatusUnauthorized {
				t.Fatalf("status = %d, want 401", response.StatusCode)
			}
			if detail := decodeProblem(t, response); detail.Type != problem.TypeUnauthorized {
				t.Errorf("type = %q, want %q", detail.Type, problem.TypeUnauthorized)
			}
		})
	}
}

func TestRoutesReachTheOwningUpstream(t *testing.T) {
	h := newHarness(t, nil)

	cases := []struct {
		method    string
		path      string
		upstream  string
		authToken string
	}{
		{http.MethodGet, "/v1/entities/8f3c", "geodata", ""},
		{http.MethodGet, "/v1/tiles/12/2048/1361.mvt", "geodata", ""},
		{http.MethodGet, "/v1/qsos/9d4c", "activity", ""},
		{http.MethodGet, "/v1/programmes", "core", ""},
		{http.MethodGet, "/v1/awards", "activity", ""},
	}
	for _, c := range cases {
		t.Run(c.path, func(t *testing.T) {
			for _, fake := range h.upstream {
				fake.calls = nil
			}
			response := h.do(t, c.method, c.path, nil)
			if response.StatusCode != http.StatusOK {
				t.Fatalf("%s %s = %d, want 200", c.method, c.path, response.StatusCode)
			}
			if got := h.upstream[c.upstream].calls; len(got) != 1 || got[0] != c.path {
				t.Errorf("%s received %v, want %s", c.upstream, got, c.path)
			}
			for name, fake := range h.upstream {
				if name != c.upstream && len(fake.calls) != 0 {
					t.Errorf("%s received %v, which belongs to %s", name, fake.calls, c.upstream)
				}
			}
		})
	}
}

func TestOpenAPIIsServed(t *testing.T) {
	h := newHarness(t, nil)

	response := h.do(t, http.MethodGet, "/openapi.json", nil)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", response.StatusCode)
	}
	if got := response.Header.Get("Content-Type"); !strings.HasPrefix(got, "application/json") {
		t.Errorf("Content-Type = %q, want JSON", got)
	}
	var document map[string]any
	if err := json.NewDecoder(response.Body).Decode(&document); err != nil {
		t.Fatalf("the contract is not JSON: %v", err)
	}
	if document["openapi"] != "3.1.0" {
		t.Errorf("openapi = %v, want 3.1.0", document["openapi"])
	}
	if paths, ok := document["paths"].(map[string]any); !ok || len(paths) == 0 {
		t.Error("the contract has no paths")
	}

	// Conditional requests: the contract changes only with a new binary, so a client
	// can revalidate instead of refetching 250 KB.
	etag := response.Header.Get("ETag")
	if etag == "" {
		t.Fatal("no ETag on the contract")
	}
	second := h.do(t, http.MethodGet, "/openapi.json", map[string]string{"If-None-Match": etag})
	if second.StatusCode != http.StatusNotModified {
		t.Errorf("conditional GET = %d, want 304", second.StatusCode)
	}
}

func TestDocsRendersWithoutAThirdParty(t *testing.T) {
	h := newHarness(t, nil)

	response := h.do(t, http.MethodGet, "/docs", nil)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", response.StatusCode)
	}
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("reading the page: %v", err)
	}
	page := string(body)

	// Every asset the page references must be same-origin. This is the acceptance
	// criterion, checked against the page rather than against a list of URLs someone
	// remembered to keep in sync.
	reference := regexp.MustCompile(`(?:src|href)="([^"]+)"`)
	references := reference.FindAllStringSubmatch(page, -1)
	if len(references) == 0 {
		t.Fatal("the page references no assets at all")
	}
	for _, match := range references {
		url := match[1]
		if strings.HasPrefix(url, "//") || strings.HasPrefix(url, "http://") || strings.HasPrefix(url, "https://") {
			t.Errorf("the page references a third party: %s", url)
		}
		if !strings.HasPrefix(url, "/") {
			t.Errorf("the page references %q, which is not an absolute same-origin path", url)
		}
	}

	// The policy is what makes the claim enforceable rather than aspirational.
	csp := response.Header.Get("Content-Security-Policy")
	for _, required := range []string{"script-src 'self'", "connect-src 'self'", "frame-ancestors 'none'"} {
		if !strings.Contains(csp, required) {
			t.Errorf("Content-Security-Policy is missing %q: %s", required, csp)
		}
	}
	if strings.Contains(csp, "unsafe-inline") && strings.Contains(csp, "script-src") {
		// `style-src 'unsafe-inline'` is expected — Scalar injects a style element. An
		// inline *script* allowance would defeat the policy.
		scriptDirective := csp[strings.Index(csp, "script-src"):]
		if end := strings.IndexByte(scriptDirective, ';'); end >= 0 {
			scriptDirective = scriptDirective[:end]
		}
		if strings.Contains(scriptDirective, "unsafe-inline") {
			t.Errorf("script-src allows inline script: %s", scriptDirective)
		}
	}

	// Both scripts the page loads must actually be served.
	for _, path := range []string{"/docs/scalar.js", "/docs/boot.js"} {
		asset := h.do(t, http.MethodGet, path, nil)
		if asset.StatusCode != http.StatusOK {
			t.Errorf("GET %s = %d, want 200", path, asset.StatusCode)
		}
		if got := asset.Header.Get("Content-Type"); !strings.HasPrefix(got, "text/javascript") {
			t.Errorf("GET %s Content-Type = %q, want JavaScript", path, got)
		}
	}
}

func TestScalarBundleIsGzippedWhenAccepted(t *testing.T) {
	h := newHarness(t, nil)

	plain := h.do(t, http.MethodGet, "/docs/scalar.js", nil)
	plainBody, _ := io.ReadAll(plain.Body)
	if plain.Header.Get("Content-Encoding") != "" {
		t.Error("the bundle was compressed for a client that did not ask")
	}

	// net/http transparently decompresses when it set Accept-Encoding itself, so the
	// header has to be sent explicitly to observe the compressed path.
	request, _ := http.NewRequest(http.MethodGet, h.server.URL+"/docs/scalar.js", nil)
	request.Header.Set("Accept-Encoding", "gzip")
	response, err := h.server.Client().Transport.RoundTrip(request)
	if err != nil {
		t.Fatalf("requesting the bundle: %v", err)
	}
	defer func() {
		// Drained before closing: an abrupt close makes the server log a write error,
		// and noise in a green test run is how people stop reading failures.
		_, _ = io.Copy(io.Discard, response.Body)
		_ = response.Body.Close()
	}()
	if response.Header.Get("Content-Encoding") != "gzip" {
		t.Fatalf("Content-Encoding = %q, want gzip", response.Header.Get("Content-Encoding"))
	}
	if response.ContentLength >= 0 && response.ContentLength >= int64(len(plainBody)) {
		t.Errorf("compressed length %d is not smaller than %d", response.ContentLength, len(plainBody))
	}
	if response.Header.Get("Vary") != "Accept-Encoding" {
		t.Error("no Vary: Accept-Encoding, so a cache could serve gzip to a client that cannot read it")
	}
}

func TestEventSchemasAreServed(t *testing.T) {
	h := newHarness(t, nil)

	index := h.do(t, http.MethodGet, "/event-schemas", nil)
	if index.StatusCode != http.StatusOK {
		t.Fatalf("GET /event-schemas = %d, want 200", index.StatusCode)
	}
	var catalogue struct {
		Events map[string]string `json:"events"`
	}
	if err := json.NewDecoder(index.Body).Decode(&catalogue); err != nil {
		t.Fatalf("the event index is not JSON: %v", err)
	}
	if len(catalogue.Events) == 0 {
		t.Fatal("the event index is empty")
	}

	for eventType := range catalogue.Events {
		response := h.do(t, http.MethodGet, "/event-schemas/"+eventType, nil)
		if response.StatusCode != http.StatusOK {
			t.Errorf("GET /event-schemas/%s = %d, want 200", eventType, response.StatusCode)
		}
	}

	// A traversal attempt is a 404 from the name check, and never reaches the
	// embedded filesystem.
	for _, attempt := range []string{
		"/event-schemas/..%2f..%2fopenapi%2fopenapi.json",
		"/event-schemas/index",
		"/event-schemas/.",
	} {
		response := h.do(t, http.MethodGet, attempt, nil)
		if response.StatusCode != http.StatusNotFound {
			t.Errorf("GET %s = %d, want 404", attempt, response.StatusCode)
		}
	}
}

func TestHealthAndReadiness(t *testing.T) {
	h := newHarness(t, nil)

	health := h.do(t, http.MethodGet, "/healthz", nil)
	if health.StatusCode != http.StatusOK {
		t.Errorf("GET /healthz = %d, want 200", health.StatusCode)
	}

	ready := h.do(t, http.MethodGet, "/readyz", nil)
	if ready.StatusCode != http.StatusOK {
		t.Errorf("GET /readyz = %d, want 200", ready.StatusCode)
	}
	var body readinessBody
	if err := json.NewDecoder(ready.Body).Decode(&body); err != nil {
		t.Fatalf("readyz is not JSON: %v", err)
	}
	if body.Status != "ok" {
		t.Errorf("status = %q, want ok", body.Status)
	}
	for _, name := range []string{"core", "geodata", "activity"} {
		if body.Upstreams[name] != "ok" {
			t.Errorf("upstreams[%s] = %q, want ok", name, body.Upstreams[name])
		}
	}
}

func TestReadinessDegradesWithoutTakingTheGatewayOutOfService(t *testing.T) {
	// One upstream down: 200 with status degraded. Three down: 503. A single backend
	// being restarted must not remove the gateway from its Service, because the routes
	// served by the other two still work.
	h := newHarness(t, nil)
	h.upstream["geodata"].server.Close()

	response := h.do(t, http.MethodGet, "/readyz", nil)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200 with one upstream down", response.StatusCode)
	}
	var body readinessBody
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		t.Fatalf("readyz is not JSON: %v", err)
	}
	if body.Status != "degraded" {
		t.Errorf("status = %q, want degraded", body.Status)
	}
	if body.Upstreams["geodata"] != "unreachable" {
		t.Errorf("upstreams[geodata] = %q, want unreachable", body.Upstreams["geodata"])
	}
}

func TestReadinessReportsMissingVerificationKey(t *testing.T) {
	// A gateway with no key still serves the public API; it must say so rather than
	// look healthy while refusing every administrator request.
	h := newHarness(t, func(settings *harnessConfig) {
		settings.config.JWTIssuer = ""
	})

	response := h.do(t, http.MethodGet, "/readyz", nil)
	var body readinessBody
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		t.Fatalf("readyz is not JSON: %v", err)
	}
	if body.Verification == "configured" {
		t.Error("readyz claims verification is configured with no issuer")
	}

	// And the admin surface refuses rather than trusting an unverified claim.
	admin := h.do(t, http.MethodGet, "/v1/admin/identity/participants", nil)
	if admin.StatusCode != http.StatusUnauthorized {
		t.Errorf("admin route without a verifier = %d, want 401", admin.StatusCode)
	}
}

func TestMetricsAreExposed(t *testing.T) {
	h := newHarness(t, nil)
	_ = h.do(t, http.MethodGet, "/v1/entities/8f3c", nil)
	_ = h.do(t, http.MethodGet, "/nope", nil)
	// The health gauge is published by a probe, so it exists only after one has run.
	// That is deliberate: a 0 before any probe would claim "unhealthy" when the truth
	// is "not checked yet".
	_ = h.do(t, http.MethodGet, "/readyz", nil)

	response := h.do(t, http.MethodGet, "/metrics", nil)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("GET /metrics = %d, want 200", response.StatusCode)
	}
	body, _ := io.ReadAll(response.Body)
	text := string(body)

	for _, series := range []string{
		"gateway_requests_total",
		"gateway_request_duration_seconds_bucket",
		"gateway_upstream_healthy",
		"go_goroutines",
	} {
		if !strings.Contains(text, series) {
			t.Errorf("metrics output has no %s", series)
		}
	}
	// The route label is the prefix, not the path: one series per entity id would be
	// an unbounded cardinality explosion.
	if !strings.Contains(text, `route="/v1/entities/"`) {
		t.Errorf("no series for the /v1/entities/ route; output:\n%s", firstLines(text, 40))
	}
	if strings.Contains(text, `/v1/entities/8f3c`) {
		t.Error("a request path became a metric label, which is unbounded cardinality")
	}
}

func TestUpstream5xxIsNormalised(t *testing.T) {
	h := newHarness(t, nil)
	h.upstream["geodata"].status = http.StatusBadGateway
	h.upstream["geodata"].body = "<html>bad gateway</html>"

	response := h.do(t, http.MethodGet, "/v1/entities/8f3c", nil)
	if response.StatusCode != http.StatusBadGateway {
		t.Errorf("status = %d, want the 502 preserved", response.StatusCode)
	}
	if got := response.Header.Get("Content-Type"); got != "application/problem+json" {
		t.Errorf("Content-Type = %q, want problem+json", got)
	}
	body, _ := io.ReadAll(response.Body)
	if strings.Contains(string(body), "<html>") {
		t.Errorf("body still contains the upstream's HTML: %s", body)
	}
}

func TestRequestIDIsEchoedAndForwarded(t *testing.T) {
	h := newHarness(t, nil)
	const supplied = "9f2c1e00-0000-4000-8000-000000000001"

	response := h.do(t, http.MethodGet, "/v1/programmes", map[string]string{"X-Request-ID": supplied})
	if got := response.Header.Get("X-Request-ID"); got != supplied {
		t.Errorf("response X-Request-ID = %q, want %q", got, supplied)
	}
}

func TestBodyLimitRefusesAnOversizedBody(t *testing.T) {
	h := newHarness(t, nil)

	request, _ := http.NewRequest(http.MethodPost, h.server.URL+"/v1/proposals",
		strings.NewReader(strings.Repeat("x", int(2<<20)+1)))
	request.Header.Set("Content-Type", "application/json")
	response, err := h.server.Client().Do(request)
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	if response.StatusCode != http.StatusBadRequest {
		t.Errorf("status = %d, want 400", response.StatusCode)
	}
	if calls := h.upstream["geodata"].calls; len(calls) != 0 {
		t.Errorf("geodata received %v despite the size refusal", calls)
	}
}

// decodeProblem reads a problem detail and closes the body.
func decodeProblem(t *testing.T, response *http.Response) problem.Detail {
	t.Helper()
	defer func() { _ = response.Body.Close() }()
	var detail problem.Detail
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("reading the problem body: %v", err)
	}
	if err := json.Unmarshal(body, &detail); err != nil {
		t.Fatalf("body is not a problem detail: %s", body)
	}
	return detail
}

// firstLines trims a large body for a failure message.
func firstLines(text string, count int) string {
	lines := strings.SplitN(text, "\n", count+1)
	if len(lines) > count {
		lines = lines[:count]
	}
	return strings.Join(lines, "\n")
}
