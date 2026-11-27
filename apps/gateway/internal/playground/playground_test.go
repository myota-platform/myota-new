package playground

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/myota-platform/myota-new/apps/gateway/internal/assets"
)

func TestNewRejectsAnIncompleteEmbed(t *testing.T) {
	// A binary that cannot serve its own contract should refuse to start, rather than
	// accept traffic and answer /openapi.json with a 500.
	cases := map[string]fstest.MapFS{
		"no contract": {
			scalarPath: &fstest.MapFile{Data: []byte("var x=1")},
		},
		"no bundle": {
			contractPath: &fstest.MapFile{Data: []byte(`{"openapi":"3.1.0"}`)},
		},
		"contract is not JSON": {
			contractPath: &fstest.MapFile{Data: []byte("openapi: 3.1.0")},
			scalarPath:   &fstest.MapFile{Data: []byte("var x=1")},
		},
		"contract has no version": {
			contractPath: &fstest.MapFile{Data: []byte(`{"paths":{}}`)},
			scalarPath:   &fstest.MapFile{Data: []byte("var x=1")},
		},
	}
	for name, files := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := New(files); err == nil {
				t.Errorf("New accepted an incomplete embed (%s)", name)
			}
		})
	}
}

func TestNewAcceptsTheRealEmbed(t *testing.T) {
	reference, err := New(assets.FS)
	if err != nil {
		t.Fatalf("New(assets.FS) = %v", err)
	}
	if reference == nil {
		t.Fatal("New returned no playground")
	}
}

func TestValidEventType(t *testing.T) {
	cases := map[string]bool{
		"core.account.created.v1":     true,
		"geodata.import.completed.v1": true,
		"activity.award.issued.v1":    true,
		"v1.schema":                   false, // the envelope, not an event
		"core.account":                false, // too few parts
		"core..created.v1":            false, // empty part
		"core.account.created.v1/../": false,
		"..%2f..%2fetc%2fpasswd":      false,
		"Core.Account.Created.v1":     false, // uppercase is not the contract's shape
		"":                            false,
	}
	for eventType, want := range cases {
		if got := validEventType(eventType); got != want {
			t.Errorf("validEventType(%q) = %v, want %v", eventType, got, want)
		}
	}
}

func TestAcceptsGzip(t *testing.T) {
	cases := map[string]bool{
		"gzip":                       true,
		"deflate, gzip":              true,
		"gzip;q=1.0, identity;q=0.5": true,
		"GZIP":                       true,
		"identity":                   false,
		"":                           false,
		"gzipx":                      false,
		"br, deflate":                false,
	}
	for header, want := range cases {
		if got := acceptsGzip(header); got != want {
			t.Errorf("acceptsGzip(%q) = %v, want %v", header, got, want)
		}
	}
}

func TestContractIsServedVerbatim(t *testing.T) {
	reference, err := New(assets.FS)
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	recorder := httptest.NewRecorder()
	reference.Contract().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/openapi.json", nil))

	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", recorder.Code)
	}
	embedded, err := assets.FS.Open(contractPath)
	if err != nil {
		t.Fatalf("opening the embedded contract: %v", err)
	}
	defer func() { _ = embedded.Close() }()
	expected, _ := io.ReadAll(embedded)
	if recorder.Body.String() != string(expected) {
		t.Error("the served contract differs from the embedded one")
	}
}

func TestScalarBundleIsTheVendoredFile(t *testing.T) {
	reference, err := New(assets.FS)
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/docs/scalar.js", nil)
	reference.Scalar().ServeHTTP(recorder, request)

	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", recorder.Code)
	}
	body := recorder.Body.String()
	// The bundle defines the global the boot script calls. If a future Scalar release
	// renames it, the page breaks silently in a browser and no Go test notices —
	// so the contract between the two is asserted here.
	if !strings.Contains(body, "window.Scalar") {
		t.Error("the bundle no longer defines window.Scalar, so /docs/boot.js would fail")
	}
}

func TestBootScriptCallsTheGlobalTheBundleDefines(t *testing.T) {
	reference, err := New(assets.FS)
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	recorder := httptest.NewRecorder()
	reference.Boot().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/docs/boot.js", nil))

	boot := recorder.Body.String()
	if !strings.Contains(boot, "Scalar.createApiReference") {
		t.Error("boot.js does not call Scalar.createApiReference")
	}
	if !strings.Contains(boot, "/openapi.json") {
		t.Error("boot.js does not point at the contract")
	}
}
