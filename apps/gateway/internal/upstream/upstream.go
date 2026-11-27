// Package upstream proxies a request to the service that owns its prefix.
//
// The gateway forwards; it does not aggregate, cache, retry or transform. That
// restraint is what keeps it a router rather than a fourth service with a database
// (ADR-0001), and it is why a screen needing entities and activations makes two
// calls instead of the gateway joining them.
//
// Three things do happen here, because they are failure modes of the edge rather
// than of the domain:
//
//   - a connection that never opens becomes 503 `#upstream-unavailable`, not a
//     proxy's default 502 with an empty body;
//   - a response that arrives after the deadline becomes 504;
//   - a 5xx that is not already `problem+json` is replaced with one, so a client has
//     a single error format to parse rather than three.
package upstream

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/myota-platform/myota-new/apps/gateway/internal/problem"
	"github.com/myota-platform/myota-new/apps/gateway/internal/requestid"
)

// DefaultRequestTimeout bounds one proxied request. Ten seconds is long enough for
// an import-status read and short enough that a hung service does not hold a
// client connection, a replica's goroutine and a Traefik slot open indefinitely.
const DefaultRequestTimeout = 10 * time.Second

// probeTimeout bounds a readiness probe. Much shorter than a request: /readyz is
// polled by the kubelet and must not queue behind a slow service.
const probeTimeout = 2 * time.Second

// Service is one upstream: where it lives, how to proxy to it, and how to ask
// whether it is alive.
type Service struct {
	// Name is the contract's service name: "core", "geodata" or "activity".
	Name string
	base *url.URL
	// proxy is the shared reverse proxy. httputil.ReverseProxy is safe for
	// concurrent use and is documented to be reused rather than rebuilt per request.
	proxy *httputil.ReverseProxy
	// client is used only for probes, so a probe cannot consume the proxy's
	// connections in the transport's idle pool.
	client *http.Client

	mutex   sync.Mutex
	healthy bool
	checked time.Time
}

// New builds a Service. baseURL must be absolute and include the scheme.
func New(name, baseURL string, timeout time.Duration, transport http.RoundTripper) (*Service, error) {
	parsed, err := url.Parse(baseURL)
	if err != nil {
		return nil, fmt.Errorf("upstream %s: parsing base URL: %w", name, err)
	}
	if parsed.Scheme == "" || parsed.Host == "" {
		return nil, fmt.Errorf("upstream %s: base URL %q needs a scheme and a host", name, baseURL)
	}
	if timeout <= 0 {
		timeout = DefaultRequestTimeout
	}

	service := &Service{Name: name, base: parsed}

	// The deadline belongs to the whole exchange — headers and body — so it is
	// applied as a transport, where the cancel function has a lifetime that ends with
	// the response body rather than with Rewrite.
	if transport == nil {
		transport = http.DefaultTransport
	}
	transport = &timeoutTransport{base: transport, timeout: timeout}

	service.proxy = &httputil.ReverseProxy{
		Rewrite: func(request *httputil.ProxyRequest) {
			request.SetURL(parsed)
			// Sets X-Forwarded-For, X-Forwarded-Host and X-Forwarded-Proto from the
			// inbound request. The service sees the client's address in X-Forwarded-For
			// and Traefik in front, which is what makes an audit log meaningful.
			request.SetXForwarded()

			// The inbound request carries the id in its context, not in a header — the
			// edge middleware generates it — so it has to be written onto the outbound
			// request here. Without this line every service logs an empty request id and
			// the gateway's own id correlates with nothing.
			if id := requestid.FromContext(request.In.Context()); id != "" {
				request.Out.Header.Set(requestid.Header, id)
			}
		},
		Transport: transport,
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			writeUpstreamFailure(w, r, name, err)
		},
		ModifyResponse: normaliseServerError,
	}

	service.client = &http.Client{Timeout: probeTimeout}

	return service, nil
}

// Handler returns the proxy handler for this upstream.
func (s *Service) Handler() http.Handler { return s.proxy }

// writeUpstreamFailure maps a transport error to a problem detail.
//
// The distinction matters to a caller: 503 means the service is not there and
// retrying later is reasonable, 504 means it was there and is slow, which is a
// different escalation. Both are reported with the upstream's name so an operator
// does not have to correlate three services by port.
func writeUpstreamFailure(w http.ResponseWriter, r *http.Request, name string, err error) {
	var netErr net.Error
	timedOut := errors.As(err, &netErr) && netErr.Timeout()
	// context.DeadlineExceeded is not always wrapped in a net.Error — the context
	// cancels the dial directly when the deadline is the caller's.
	timedOut = timedOut || errors.Is(err, context.DeadlineExceeded)

	if timedOut {
		slog.WarnContext(r.Context(), "upstream timed out",
			"upstream", name, "path", r.URL.Path, "request_id", requestid.FromContext(r.Context()))
		problem.Write(w, r, problem.TypeUpstreamUnavailable, http.StatusGatewayTimeout,
			fmt.Sprintf("%s did not respond in time.", name))
		return
	}

	slog.WarnContext(r.Context(), "upstream unreachable",
		"upstream", name, "path", r.URL.Path, "error", err,
		"request_id", requestid.FromContext(r.Context()))
	problem.Write(w, r, problem.TypeUpstreamUnavailable, http.StatusServiceUnavailable,
		fmt.Sprintf("%s is unreachable.", name))
}

// normaliseServerError replaces a 5xx that is not already a problem detail.
//
// A service is supposed to answer with problem+json itself, and normally does. What
// this catches is everything else: a framework's HTML error page, a bare "upstream
// connect error" from a sidecar, an empty 502. Those reach a client as an
// unparseable body, and the client then has to guess whether the failure was theirs.
//
// The status code is preserved — 500 stays 500, 503 stays 503 — because the caller's
// retry decision depends on it.
func normaliseServerError(response *http.Response) error {
	if response.StatusCode < 500 {
		return nil
	}
	if strings.HasPrefix(response.Header.Get("Content-Type"), "application/problem+json") {
		return nil
	}

	body, err := problem.Marshal(problem.TypeUpstreamUnavailable, response.StatusCode,
		"The upstream returned an error without a problem detail.",
		response.Request.URL.Path,
		requestid.FromContext(response.Request.Context()))
	if err != nil {
		return fmt.Errorf("normalising upstream error: %w", err)
	}

	// The original body must be drained and closed or the connection is not reusable.
	_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 1<<20))
	_ = response.Body.Close()

	response.Body = io.NopCloser(bytes.NewReader(body))
	response.ContentLength = int64(len(body))
	// The header has to be corrected too, not only the field. The reverse proxy
	// copies Header onto the response, so a stale `Content-Length` from the upstream
	// makes net/http believe the handler over-wrote the body and abort the connection
	// mid-response — which reaches the client as an EOF with no status at all.
	response.Header.Set("Content-Length", strconv.Itoa(len(body)))
	response.Header.Set("Content-Type", "application/problem+json")
	response.Header.Set("X-Content-Type-Options", "nosniff")
	response.Header.Del("Content-Encoding")
	// A replacement body is never chunked, and a leftover chunked declaration would
	// conflict with the length above.
	response.TransferEncoding = nil
	return nil
}

// Probe asks the upstream whether it is alive. Results are cached for a second so a
// kubelet polling /readyz cannot turn into a request storm against three services.
func (s *Service) Probe(ctx context.Context) error {
	s.mutex.Lock()
	if time.Since(s.checked) < time.Second {
		healthy := s.healthy
		s.mutex.Unlock()
		if healthy {
			return nil
		}
		return fmt.Errorf("upstream %s: unhealthy as of %s ago", s.Name, time.Since(s.checked).Round(time.Millisecond))
	}
	s.mutex.Unlock()

	err := s.probeNow(ctx)

	s.mutex.Lock()
	s.healthy = err == nil
	s.checked = time.Now()
	s.mutex.Unlock()

	return err
}

// probeNow performs one health request. Kept separate so the cached result and the
// live check cannot be confused.
func (s *Service) probeNow(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, probeTimeout)
	defer cancel()

	// The path is in the contract as an internal operation, so this probe depends on
	// a documented endpoint rather than on an assumption about the service.
	target := s.base.JoinPath("/internal/healthz").String()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return fmt.Errorf("upstream %s: building probe: %w", s.Name, err)
	}
	response, err := s.client.Do(request)
	if err != nil {
		return fmt.Errorf("upstream %s: probe failed: %w", s.Name, err)
	}
	defer func() { _ = response.Body.Close() }()
	_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 4096))

	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("upstream %s: probe returned %s", s.Name, response.Status)
	}
	return nil
}

// timeoutTransport applies a deadline to a whole exchange.
//
// It exists because the obvious alternative — deriving a context in the proxy's
// Rewrite hook — has nowhere to call the cancel function: Rewrite returns before the
// request is made. Discarding the cancel leaks a timer per request and is exactly
// what `go vet`'s lostcancel check exists to catch.
//
// Here the cancel is tied to the response body, which the reverse proxy always
// closes. Cancelling when RoundTrip returns instead would truncate every response
// body to whatever arrived with the headers.
type timeoutTransport struct {
	base    http.RoundTripper
	timeout time.Duration
}

// RoundTrip implements http.RoundTripper.
func (t *timeoutTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	ctx, cancel := context.WithTimeout(request.Context(), t.timeout)
	response, err := t.base.RoundTrip(request.WithContext(ctx))
	if err != nil {
		timedOut := errors.Is(err, context.DeadlineExceeded) || errors.Is(ctx.Err(), context.DeadlineExceeded)
		cancel()
		if timedOut {
			// Wrapped so the error handler can tell a timeout from a refusal.
			return nil, fmt.Errorf("%w: %w", context.DeadlineExceeded, err)
		}
		return nil, err
	}
	response.Body = &bodyWithCancel{ReadCloser: response.Body, cancel: cancel}
	return response, nil
}

// bodyWithCancel cancels the exchange's context exactly once, when the body is
// closed.
type bodyWithCancel struct {
	io.ReadCloser
	once   sync.Once
	cancel context.CancelFunc
}

// Close implements io.Closer.
func (b *bodyWithCancel) Close() error {
	err := b.ReadCloser.Close()
	b.once.Do(b.cancel)
	return err
}

// Set is the collection of configured upstreams, looked up by contract service name.
type Set struct {
	byName map[string]*Service
}

// NewSet builds a Set. Every upstream the route table names must have a base URL,
// because a missing one would be a 500 on the first request to that prefix rather
// than a configuration error at startup.
func NewSet(baseURLs map[string]string, timeout time.Duration, transport http.RoundTripper) (*Set, error) {
	set := &Set{byName: make(map[string]*Service, len(baseURLs))}
	for name, baseURL := range baseURLs {
		service, err := New(name, baseURL, timeout, transport)
		if err != nil {
			return nil, err
		}
		set.byName[name] = service
	}
	return set, nil
}

// Get returns the upstream with this name.
func (s *Set) Get(name string) (*Service, bool) {
	service, ok := s.byName[name]
	return service, ok
}

// Names returns the configured upstream names, sorted so readiness output and log
// lines are stable between calls.
func (s *Set) Names() []string {
	names := make([]string, 0, len(s.byName))
	for name := range s.byName {
		names = append(names, name)
	}
	slices.Sort(names)
	return names
}
