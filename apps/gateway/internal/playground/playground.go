// Package playground serves the published contract and its interactive reference.
//
// Two routes, both public, both read-only:
//
//	GET /openapi.json          the composed contract
//	GET /docs                  the Scalar reference, rendered client-side
//
// Neither touches an upstream. The contract is embedded in the binary, so a service
// being down cannot take the API documentation with it — which matters, because the
// documentation is the thing someone reads to find out why the service is down.
//
// ## No third party
//
// The Scalar bundle is vendored (see internal/assets/playground/README.md) and
// served from this binary. A CDN reference would make an internal operator tool
// depend on an external service being reachable, and would tell that service who is
// using the cluster. A test fetches every asset the page references and fails if any
// of them leaves our origin.
package playground

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"io/fs"
	"log/slog"
	"net/http"
	"strings"
	"sync"
)

// Asset paths inside the embedded filesystem.
const (
	contractPath = "openapi/openapi.json"
	scalarPath   = "playground/standalone.js"
	eventsDir    = "events"
	eventsIndex  = "events/index.json"
)

// Playground serves the contract and the reference page from embedded assets.
type Playground struct {
	contract    []byte
	contractTag string
	assets      fs.FS

	// scalarJS is compressed once. Compressing 4.5 MB per request would cost more
	// CPU than serving it, and the bundle never changes while the process runs.
	scalarOnce sync.Once
	scalarJS   []byte
	scalarGzip []byte
	scalarErr  error
}

// New builds a Playground over the embedded assets. It fails at startup rather than
// per request: a gateway that cannot serve its own contract should not accept
// traffic and then answer /openapi.json with a 500.
func New(assets fs.FS) (*Playground, error) {
	contract, err := fs.ReadFile(assets, contractPath)
	if err != nil {
		return nil, fmt.Errorf("playground: reading the embedded contract: %w", err)
	}

	// Parsed once to prove it is valid JSON and to fail loudly on a broken embed.
	// The bytes served are the bytes embedded, so what was validated is what ships.
	var document map[string]any
	if err := json.Unmarshal(contract, &document); err != nil {
		return nil, fmt.Errorf("playground: the embedded contract is not valid JSON: %w", err)
	}
	version, _ := document["openapi"].(string)
	if version == "" {
		return nil, fmt.Errorf("playground: the embedded contract has no openapi version")
	}

	if _, err := fs.Stat(assets, scalarPath); err != nil {
		return nil, fmt.Errorf("playground: the Scalar bundle is missing from the binary: %w", err)
	}

	return &Playground{
		contract: contract,
		// A stable validator for If-None-Match. The contract only changes when the
		// binary is rebuilt, so its length and version are enough — a hash would add
		// cost for no benefit, since a rebuilt binary is a new ETag anyway.
		contractTag: fmt.Sprintf(`"openapi-%s-%d"`, version, len(contract)),
		assets:      assets,
	}, nil
}

// Contract serves the composed contract as JSON.
func (p *Playground) Contract() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json; charset=utf-8")
		w.Header().Set("ETag", p.contractTag)
		// Not immutable: the gateway is redeployed, and a client that cached the old
		// contract must revalidate rather than hold an API description that no longer
		// matches what the deployment serves.
		w.Header().Set("Cache-Control", "public, max-age=300")
		if match := r.Header.Get("If-None-Match"); match == p.contractTag {
			w.WriteHeader(http.StatusNotModified)
			return
		}
		if _, err := w.Write(p.contract); err != nil {
			slog.ErrorContext(r.Context(), "writing the contract", "error", err)
		}
	})
}

// Docs serves the reference page.
//
// The page is generated rather than embedded as a file so the contract URL and the
// event-schema link come from one place, and so a templating mistake is a compile
// error rather than a broken page discovered in a browser.
func (p *Playground) Docs() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		// The page embeds no data of its own and pulls the contract at runtime, so a
		// stale cache is harmless — but the script URL must stay stable for that to be
		// true, which is why it is a fixed path rather than a content hash.
		w.Header().Set("Cache-Control", "no-cache")
		w.Header().Set("Referrer-Policy", "no-referrer")
		// Two policies, one purpose. `script-src 'self'` with no `'unsafe-inline'` and
		// no nonce means the page can execute only scripts this binary serves, and
		// `connect-src 'self'` means the browser refuses to send the contract fetch
		// anywhere else. Together they turn "no third party" from a claim into
		// something the browser enforces — which is why the initialisation lives in
		// /docs/boot.js rather than in an inline block.
		w.Header().Set("Content-Security-Policy",
			"default-src 'self'; "+
				"script-src 'self'; "+
				"style-src 'self' 'unsafe-inline'; "+
				"img-src 'self' data:; "+
				"font-src 'self' data:; "+
				"connect-src 'self'; "+
				"frame-ancestors 'none'")
		if _, err := w.Write([]byte(docsPage)); err != nil {
			slog.ErrorContext(r.Context(), "writing the docs page", "error", err)
		}
	})
}

// Scalar serves the vendored Scalar bundle.
//
// Gzip when the client accepts it — the bundle is 4.5 MB raw and 1.35 MB compressed,
// which is worth the once-per-process cost. The client's `Accept-Encoding` is read
// rather than assumed, because serving gzip to a client that did not ask for it is
// an unparseable response.
func (p *Playground) Scalar() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		p.scalarOnce.Do(func() {
			p.scalarJS, p.scalarErr = fs.ReadFile(p.assets, scalarPath)
			if p.scalarErr != nil {
				return
			}
			var buffer bytes.Buffer
			writer, err := gzip.NewWriterLevel(&buffer, gzip.BestCompression)
			if err != nil {
				p.scalarErr = err
				return
			}
			if _, err := writer.Write(p.scalarJS); err != nil {
				p.scalarErr = err
				return
			}
			if err := writer.Close(); err != nil {
				p.scalarErr = err
				return
			}
			p.scalarGzip = buffer.Bytes()
		})
		if p.scalarErr != nil {
			slog.ErrorContext(r.Context(), "reading the Scalar bundle", "error", p.scalarErr)
			http.Error(w, "the reference bundle is unavailable", http.StatusInternalServerError)
			return
		}

		w.Header().Set("Content-Type", "text/javascript; charset=utf-8")
		// Safe to cache hard: the path changes only with a new binary, and a new binary
		// is a new deployment.
		w.Header().Set("Cache-Control", "public, max-age=86400, immutable")

		if acceptsGzip(r.Header.Get("Accept-Encoding")) {
			w.Header().Set("Content-Encoding", "gzip")
			w.Header().Set("Vary", "Accept-Encoding")
			if _, err := w.Write(p.scalarGzip); err != nil {
				slog.ErrorContext(r.Context(), "writing the Scalar bundle", "error", err)
			}
			return
		}
		w.Header().Set("Content-Length", fmt.Sprint(len(p.scalarJS)))
		if _, err := w.Write(p.scalarJS); err != nil {
			slog.ErrorContext(r.Context(), "writing the Scalar bundle", "error", err)
		}
	})
}

// EventIndex serves the catalogue of event schemas.
func (p *Playground) EventIndex() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		index, err := fs.ReadFile(p.assets, eventsIndex)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/json; charset=utf-8")
		if _, err := w.Write(index); err != nil {
			slog.ErrorContext(r.Context(), "writing the event index", "error", err)
		}
	})
}

// EventSchema serves one event type's JSON Schema.
//
// The type is taken from the URL and used to look up a file in the embedded
// directory. `fs.ReadFile` on an `fs.FS` cannot escape the tree, but the name is
// still checked against the pattern the contract uses, so a request for
// `/event-schemas/..%2f..%2fetc%2fpasswd` is a 404 rather than a filesystem lookup.
func (p *Playground) EventSchema() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		eventType := r.PathValue("type")
		if !validEventType(eventType) {
			http.NotFound(w, r)
			return
		}
		schema, err := fs.ReadFile(p.assets, eventsDir+"/"+eventType+".json")
		if err != nil {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/schema+json; charset=utf-8")
		if _, err := w.Write(schema); err != nil {
			slog.ErrorContext(r.Context(), "writing an event schema", "error", err)
		}
	})
}

// validEventType reports whether s looks like `<service>.<aggregate>.<fact>.v<n>`.
//
// Deliberately strict, because this string becomes a filename. An `fs.FS` cannot
// escape its tree, so this is defence in depth rather than the only barrier — but a
// traversal attempt should be a 404 from a name check, not a filesystem call that
// merely happens to be safe.
func validEventType(s string) bool {
	if s == "" || len(s) > 120 || strings.Contains(s, "..") {
		return false
	}
	parts := strings.Split(s, ".")
	if len(parts) < 4 {
		return false
	}
	for _, part := range parts {
		if part == "" {
			return false
		}
		for _, char := range part {
			if (char < 'a' || char > 'z') && (char < '0' || char > '9') && char != '_' && char != '-' {
				return false
			}
		}
	}
	return true
}

// bootScript mounts Scalar. It is a served file rather than an inline block so the
// page needs neither `'unsafe-inline'` nor a per-request nonce, and so
// `script-src 'self'` in the page's policy is literally true.
const bootScript = `Scalar.createApiReference('#app', {
  url: '/openapi.json',
  withDefaultFonts: false,
  defaultHttpClient: { targetKey: 'shell', clientKey: 'curl' }
});
`

// Boot serves the mount script.
func (p *Playground) Boot() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/javascript; charset=utf-8")
		w.Header().Set("Cache-Control", "no-cache")
		if _, err := w.Write([]byte(bootScript)); err != nil {
			slog.ErrorContext(r.Context(), "writing the boot script", "error", err)
		}
	})
}

// acceptsGzip reports whether an Accept-Encoding header offers gzip.
func acceptsGzip(header string) bool {
	for _, part := range strings.Split(header, ",") {
		encoding := strings.TrimSpace(part)
		if index := strings.IndexByte(encoding, ';'); index >= 0 {
			encoding = strings.TrimSpace(encoding[:index])
		}
		if strings.EqualFold(encoding, "gzip") {
			return true
		}
	}
	return false
}

// docsPage is the reference page. Kept as a constant rather than a template file
// because it has no data: the contract is fetched by the browser from
// /openapi.json, so the page is identical for every request and every deployment.
const docsPage = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Myota API reference</title>
<meta name="robots" content="noindex">
<style>
  html, body { margin: 0; padding: 0; }
  .myota-banner {
    font: 13px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    display: flex; gap: 1.5rem; flex-wrap: wrap; align-items: baseline;
    padding: 0.6rem 1.25rem; border-bottom: 1px solid #e2e2e2;
    background: #fafafa; color: #444;
  }
  .myota-banner strong { color: #111; font-weight: 600; }
  .myota-banner a { color: #0b5cad; text-decoration: none; }
  .myota-banner a:hover { text-decoration: underline; }
  .myota-banner .sep { color: #bbb; }
  @media (prefers-color-scheme: dark) {
    .myota-banner { background: #161616; border-bottom-color: #2a2a2a; color: #bbb; }
    .myota-banner strong { color: #f2f2f2; }
    .myota-banner a { color: #6cb2f0; }
  }
</style>
</head>
<body>
<nav class="myota-banner">
  <strong>Myota API</strong>
  <span class="sep">·</span>
  <a href="/openapi.json">Contract</a>
  <a href="/event-schemas">Event schemas</a>
  <span class="sep">·</span>
  <span>Two audiences: participant routes outside <code>/v1/admin</code>, administrator routes inside it.</span>
</nav>
<div id="app"></div>
<script src="/docs/scalar.js"></script>
<script src="/docs/boot.js"></script>
</body>
</html>
`
