package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"

	"github.com/myota-platform/myota-new/apps/gateway/internal/config"
	"github.com/myota-platform/myota-new/apps/gateway/internal/edge"
	"github.com/myota-platform/myota-new/apps/gateway/internal/playground"
	"github.com/myota-platform/myota-new/apps/gateway/internal/problem"
	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
	"github.com/myota-platform/myota-new/apps/gateway/internal/routes"
	"github.com/myota-platform/myota-new/apps/gateway/internal/session"
	"github.com/myota-platform/myota-new/apps/gateway/internal/upstream"
)

// ServiceName is used in logs, in /healthz and in metrics. It matches the image and
// the Kubernetes workload name, so a log line names what kubectl names.
const ServiceName = "gateway"

// Cookie names from the contract. The prefix `__Host-` forces Secure, forbids
// Domain and pins the path, which is what makes the two audiences unable to
// overwrite each other in one browser.
const (
	adminSessionCookie       = "__Host-myota_admin_sid"
	participantSessionCookie = "__Host-myota_sid"
)

// Gateway is the assembled edge: routes, middleware, upstreams and the playground.
type Gateway struct {
	config     config.Config
	upstreams  *upstream.Set
	verifier   *session.Verifier
	playground *playground.Playground
	metrics    *Metrics
	handler    http.Handler
}

// New assembles a Gateway. Every dependency is passed in rather than constructed
// here, so a test can hand it a key source and a set of upstreams without touching
// the environment.
func New(
	configuration config.Config,
	upstreams *upstream.Set,
	verifier *session.Verifier,
	reference *playground.Playground,
	metrics *Metrics,
) *Gateway {
	if metrics == nil {
		metrics = NewMetrics()
	}
	gateway := &Gateway{
		config:     configuration,
		upstreams:  upstreams,
		verifier:   verifier,
		playground: reference,
		metrics:    metrics,
	}
	gateway.handler = gateway.buildRouter()
	return gateway
}

// Handler returns the HTTP handler, ready for http.Server.
func (g *Gateway) Handler() http.Handler { return g.handler }

// Metrics exposes the registry so main can serve it and a test can read it.
func (g *Gateway) Metrics() *Metrics { return g.metrics }

// buildRouter wires the whole edge. Middleware order is meaningful and is explained
// where each layer is added.
func (g *Gateway) buildRouter() http.Handler {
	router := chi.NewRouter()

	// Outermost: Recover must wrap RequestID so a panic inside it is still caught,
	// and RequestID must wrap everything that logs so every line carries the id.
	router.Use(edge.Recover)
	router.Use(edge.RequestID)
	router.Use(edge.TraceParent)
	router.Use(g.instrument)

	// The gateway's own surface. None of it touches an upstream.
	router.Get("/openapi.json", g.playground.Contract().ServeHTTP)
	router.Get("/docs", g.playground.Docs().ServeHTTP)
	router.Get("/docs/scalar.js", g.playground.Scalar().ServeHTTP)
	router.Get("/docs/boot.js", g.playground.Boot().ServeHTTP)
	router.Get("/event-schemas", g.playground.EventIndex().ServeHTTP)
	router.Get("/event-schemas/{type}", g.playground.EventSchema().ServeHTTP)

	router.Get("/healthz", g.health)
	router.Get("/readyz", g.ready)
	router.Handle("/metrics", g.metrics.Handler())

	// Everything else is a candidate for proxying. The lookup, not a chi pattern,
	// decides: the route table is generated from the contract, and chi's patterns
	// would be a second copy of it.
	router.NotFound(g.serveAPI)

	return router
}

// instrument records request count and latency per matched route.
func (g *Gateway) instrument(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		started := time.Now()
		recorder := &statusRecorder{ResponseWriter: w}
		next.ServeHTTP(recorder, r)

		route, ok := routes.Lookup(r.URL.Path)
		label := "unmatched"
		if ok {
			label = route.Prefix
		}
		g.metrics.observe(label, r.Method, recorder.Status(), time.Since(started))
	})
}

// serveAPI proxies a request to the service that owns its prefix, or answers 404.
//
// The order of the checks is the policy:
//
//  1. no route  → 404. Never a guessed upstream, never a default service.
//  2. audience  → 401 before the request reaches a service, so an administrator
//     route is not reachable with a participant credential even if the service
//     forgot to check.
//  3. body size → 413 before anything is read.
//  4. proxy.
func (g *Gateway) serveAPI(w http.ResponseWriter, r *http.Request) {
	route, found := routes.Lookup(r.URL.Path)
	if !found {
		problem.WriteStatus(w, r, problem.TypeNotFound,
			fmt.Sprintf("No API route matches %s.", r.URL.Path))
		return
	}

	if route.Audience == routes.AudienceAdmin && !g.authoriseAdmin(w, r, route) {
		return
	}

	service, ok := g.upstreams.Get(route.Upstream)
	if !ok {
		// Configuration is checked at startup, so this means the route table and the
		// configured upstreams disagree. A 500 is correct: the gateway is misbuilt.
		slog.ErrorContext(r.Context(), "no upstream for route",
			"prefix", route.Prefix, "upstream", route.Upstream)
		problem.WriteStatus(w, r, problem.TypeInternal,
			fmt.Sprintf("The gateway has no upstream configured for %s.", route.Upstream))
		return
	}

	// Only methods that carry a body are limited; a GET with a 2 MiB body is refused
	// by the upstream's own rules, and applying the limit here would need a rule about
	// which GETs may have one.
	if r.ContentLength != 0 {
		edge.BodyLimit(edge.MaxJSONBodyBytes)(service.Handler()).ServeHTTP(w, r)
		return
	}
	service.Handler().ServeHTTP(w, r)
}

// authoriseAdmin enforces the administrator audience. It returns false when it has
// already written a response.
//
// Three cases, and the third is the one worth understanding:
//
//   - A **bearer token** is verified here. Wrong audience is 401 `#wrong-audience`;
//     an unverifiable token is 401 `#unauthorized`. This is the acceptance criterion.
//   - A **participant session cookie** is a wrong-audience request by itself. The
//     cookie names encode the audience, and refusing here is what turns the cookie
//     split from a client-side convention into a server-side control.
//   - An **administrator session cookie with no bearer** is forwarded: the cookie is
//     an opaque refresh credential that only `core` can verify, and `core` checks the
//     session's audience. The gateway does not pretend to verify what it cannot.
func (g *Gateway) authoriseAdmin(w http.ResponseWriter, r *http.Request, route routes.Route) bool {
	if token := bearerToken(r); token != "" {
		identity, err := g.verifier.Verify(r.Context(), token)
		if err != nil {
			if errors.Is(err, session.ErrNoKey) {
				// The gateway has no key material: it cannot tell an administrator from
				// anyone else. Refusing is the only safe answer, and the log line tells
				// the operator what to fix.
				slog.ErrorContext(r.Context(), "cannot verify administrator token",
					"error", err, "request_id", requestid.FromContext(r.Context()))
				problem.WriteStatus(w, r, problem.TypeUnauthorized,
					"The gateway has no token verification key configured.")
				return false
			}
			problem.WriteStatus(w, r, problem.TypeUnauthorized, "The access token is not valid.")
			return false
		}
		if identity.Audience != session.AudienceAdmin {
			problem.WriteStatus(w, r, problem.TypeWrongAudience,
				fmt.Sprintf("This is a %s session; the route requires an administrator session.", identity.Audience))
			return false
		}
		return true
	}

	// A participant cookie is a wrong-audience request on its face. The two cookies
	// have different names precisely so the server can tell them apart; until now the
	// distinction lived only in the browser.
	if _, err := r.Cookie(participantSessionCookie); err == nil {
		problem.WriteStatus(w, r, problem.TypeWrongAudience,
			"A participant session cannot be used on an administrator route.")
		return false
	}

	// An administrator refresh cookie with no bearer token: `core` owns the session
	// table and decides whether it is still valid and still an administrator's.
	if _, err := r.Cookie(adminSessionCookie); err == nil {
		return true
	}

	if route.AllowsWithoutToken(r.URL.Path) {
		return true
	}
	problem.WriteStatus(w, r, problem.TypeUnauthorized,
		"An administrator session is required for this route.")
	return false
}

// bearerToken extracts the token from an Authorization header, or returns "".
func bearerToken(r *http.Request) string {
	header := r.Header.Get("Authorization")
	if len(header) < 7 || !strings.EqualFold(header[:7], "Bearer ") {
		return ""
	}
	return strings.TrimSpace(header[7:])
}

// health reports liveness. It is deliberately trivial: it answers "this process is
// running", and a deployment should restart the pod only when that is false.
func (g *Gateway) health(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, r, http.StatusOK, map[string]string{
		"status":  "ok",
		"service": ServiceName,
	})
}

// readinessBody is what /readyz returns.
type readinessBody struct {
	Status string `json:"status"`
	// Upstreams is per-service so an operator sees which one is down without
	// correlating three Services and a DNS name.
	Upstreams map[string]string `json:"upstreams"`
	// Verification says whether the gateway can check a token at all. It is not a
	// readiness failure — the public API works without it — but it explains a 401 on
	// every administrator request.
	Verification string `json:"verification"`
}

// ready reports whether the gateway can serve.
//
// It returns 503 only when *every* upstream is unreachable. One backend being down
// must not remove the gateway from its Service: the routes served by the other two
// still work, and taking the whole ingress path offline because `geodata` is
// restarting turns a partial outage into a total one. The body says which upstreams
// are down so a degraded state is visible rather than inferred from a 200.
func (g *Gateway) ready(w http.ResponseWriter, r *http.Request) {
	body := readinessBody{
		Status:       "ok",
		Upstreams:    make(map[string]string, len(g.config.Upstreams)),
		Verification: "configured",
	}
	if g.verifier == nil || !g.verifier.Configured() {
		body.Verification = "no verification key: administrator routes will refuse every request"
	}

	reachable := 0
	down := make([]string, 0, len(g.config.Upstreams))
	for _, name := range g.upstreams.Names() {
		service, _ := g.upstreams.Get(name)

		// Each probe gets its own short deadline so three slow upstreams cannot add up
		// to a readiness check the kubelet times out on.
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		err := service.Probe(ctx)
		cancel()

		if err != nil {
			body.Upstreams[name] = "unreachable"
			down = append(down, name)
			g.metrics.setUpstreamHealth(name, false)
			continue
		}
		body.Upstreams[name] = "ok"
		reachable++
		g.metrics.setUpstreamHealth(name, true)
	}

	switch {
	case reachable == 0 && len(g.config.Upstreams) > 0:
		body.Status = "unavailable"
		writeJSON(w, r, http.StatusServiceUnavailable, body)
	case len(down) > 0:
		body.Status = "degraded"
		writeJSON(w, r, http.StatusOK, body)
	default:
		writeJSON(w, r, http.StatusOK, body)
	}
}

// writeJSON writes a JSON body with the status code.
func writeJSON(w http.ResponseWriter, r *http.Request, status int, body any) {
	encoded, err := json.Marshal(body)
	if err != nil {
		slog.ErrorContext(r.Context(), "encoding a JSON response", "error", err)
		w.WriteHeader(http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(status)
	if _, err := w.Write(encoded); err != nil {
		slog.ErrorContext(r.Context(), "writing a JSON response", "error", err)
	}
}
