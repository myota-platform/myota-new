package gateway

import (
	"net/http"
	"strconv"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// Metrics is the gateway's Prometheus instrumentation.
//
// A per-instance registry, not the package-global default, so a test can build a
// gateway without a second one panicking on a duplicate registration — and so the
// exported set is exactly what this file declares, rather than everything any
// imported package happened to register.
//
// Cardinality is bounded deliberately. The `route` label is the matched prefix — 22
// values — plus `unmatched`; labelling by request path would make one series per
// entity id, which is how a Prometheus instance falls over.
type Metrics struct {
	registry *prometheus.Registry
	requests *prometheus.CounterVec
	duration *prometheus.HistogramVec
	healthy  *prometheus.GaugeVec
}

// NewMetrics builds an empty registry with the gateway's collectors registered.
func NewMetrics() *Metrics {
	registry := prometheus.NewRegistry()
	metrics := &Metrics{
		registry: registry,
		requests: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "gateway_requests_total",
			Help: "Requests handled, by matched route prefix, method and status code.",
		}, []string{"route", "method", "code"}),
		duration: prometheus.NewHistogramVec(prometheus.HistogramOpts{
			Name: "gateway_request_duration_seconds",
			Help: "Time from accepting a request to finishing the response.",
			// Tuned to the 10 s upstream deadline: most buckets are below a second,
			// and the tail makes a timing-out upstream visible rather than lumped into
			// +Inf.
			Buckets: []float64{0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10},
		}, []string{"route"}),
		healthy: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "gateway_upstream_healthy",
			Help: "1 when the last readiness probe of an upstream succeeded, 0 otherwise.",
		}, []string{"upstream"}),
	}
	registry.MustRegister(metrics.requests, metrics.duration, metrics.healthy)
	// Go runtime and process collectors: GC pause and open file descriptors are the
	// two things that explain a gateway that is up but slow, and they cost nothing.
	registry.MustRegister(prometheus.NewGoCollector(), prometheus.NewProcessCollector(prometheus.ProcessCollectorOpts{}))
	return metrics
}

// Handler serves the registry in the Prometheus text format.
func (m *Metrics) Handler() http.Handler {
	return promhttp.HandlerFor(m.registry, promhttp.HandlerOpts{
		// A scrape that fails halfway is more useful as a partial page than as a 500:
		// the operator still gets every series that was collected.
		ErrorHandling: promhttp.ContinueOnError,
	})
}

// observe records one completed request.
func (m *Metrics) observe(route, method string, code int, elapsed time.Duration) {
	m.requests.WithLabelValues(route, method, strconv.Itoa(code)).Inc()
	m.duration.WithLabelValues(route).Observe(elapsed.Seconds())
}

// setUpstreamHealth publishes a probe result. Only called by the readiness handler,
// so the gauge always reflects the most recent probe rather than an average.
func (m *Metrics) setUpstreamHealth(upstream string, healthy bool) {
	value := 0.0
	if healthy {
		value = 1.0
	}
	m.healthy.WithLabelValues(upstream).Set(value)
}

// statusRecorder captures the status code a handler wrote.
//
// net/http offers no way to read it back afterwards, and every metric here needs it.
// The zero value means "nothing was written", which net/http reports as 200 — so
// WriteHeader is the only place the code is recorded and the default is set at the
// same time.
type statusRecorder struct {
	http.ResponseWriter
	code        int
	wroteHeader bool
	// bytes is not exported; it exists so an extra Write does not have to be
	// intercepted again if a future metric wants it.
	bytes int
}

// WriteHeader implements http.ResponseWriter.
func (r *statusRecorder) WriteHeader(code int) {
	if !r.wroteHeader {
		r.code = code
		r.wroteHeader = true
	}
	r.ResponseWriter.WriteHeader(code)
}

// Write implements http.ResponseWriter.
func (r *statusRecorder) Write(body []byte) (int, error) {
	if !r.wroteHeader {
		r.WriteHeader(http.StatusOK)
	}
	written, err := r.ResponseWriter.Write(body)
	r.bytes += written
	return written, err
}

// Status returns the status code that was written, or 200 when the handler wrote a
// body without calling WriteHeader.
func (r *statusRecorder) Status() int {
	if !r.wroteHeader {
		return http.StatusOK
	}
	return r.code
}

// Flush forwards to the wrapped writer when it supports flushing, so a proxied
// streaming response is not held in a buffer by this wrapper.
func (r *statusRecorder) Flush() {
	if flusher, ok := r.ResponseWriter.(http.Flusher); ok {
		flusher.Flush()
	}
}
