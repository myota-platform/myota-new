package routes

import "testing"

func TestLookupFindsTheOwningPrefix(t *testing.T) {
	cases := []struct {
		path     string
		upstream string
		ok       bool
	}{
		{"/v1/entities/8f3c1e00-0000-4000-8000-000000000001", "geodata", true},
		{"/v1/entities/", "geodata", true},
		{"/v1/activations/1b2d4e60/qsos", "activity", true},
		{"/v1/admin/identity/participants", "core", true},
		{"/v1/admin/award-requests/4a9c/approve", "activity", true},
		{"/v1/admin/sources/1c5e/timeline", "geodata", true},
		// Not a prefix the contract declares: never proxied, never guessed.
		{"/v1/unknown", "", false},
		{"/v2/entities/", "", false},
		{"/", "", false},
		{"/docs", "", false},
		{"/openapi.json", "", false},
		// A prefix that is a prefix of a prefix must not match: /v1/admin is not a
		// route even though /v1/admin/auth/ is.
		{"/v1/admin", "", false},
		{"/v1/admins/1", "", false},
	}
	for _, c := range cases {
		t.Run(c.path, func(t *testing.T) {
			route, ok := Lookup(c.path)
			if ok != c.ok {
				t.Fatalf("Lookup(%q) found = %v, want %v", c.path, ok, c.ok)
			}
			if ok && route.Upstream != c.upstream {
				t.Errorf("Lookup(%q) upstream = %q, want %q", c.path, route.Upstream, c.upstream)
			}
		})
	}
}

func TestLookupPrefersTheLongestPrefix(t *testing.T) {
	// Verified against the generated table rather than a hand-built one, so the test
	// fails if generation ever emits a shadowing order.
	route, ok := Lookup("/v1/admin/award-requests/x")
	if !ok {
		t.Fatal("no route for /v1/admin/award-requests/x")
	}
	if route.Prefix != "/v1/admin/award-requests/" {
		t.Errorf("matched %q, want the longest matching prefix", route.Prefix)
	}
}

func TestAdminPrefixesRequireTheAdminAudience(t *testing.T) {
	for _, route := range Table {
		wantAdmin := len(route.Prefix) >= len("/v1/admin/") && route.Prefix[:len("/v1/admin/")] == "/v1/admin/"
		if got := route.Audience == AudienceAdmin; got != wantAdmin {
			t.Errorf("%s audience = %q; want administrator-only = %v", route.Prefix, route.Audience, wantAdmin)
		}
	}
}

func TestOnlyLoginPathsAreTokenFree(t *testing.T) {
	route, ok := Lookup("/v1/admin/auth/login")
	if !ok {
		t.Fatal("no route for /v1/admin/auth/login")
	}
	if !route.AllowsWithoutToken("/v1/admin/auth/login") {
		t.Error("login is not token-free, so an administrator could never sign in")
	}
	// The distinction that matters: the login is public, the refresh that follows it
	// is not. Same prefix, adjacent paths.
	if route.AllowsWithoutToken("/v1/admin/auth/refresh") {
		t.Error("refresh is token-free, so a participant cookie would be accepted there")
	}
	if route.AllowsWithoutToken("/v1/admin/auth/login/extra") {
		t.Error("token-free matching is exact, not a prefix")
	}
	if route.AllowsWithoutToken("/v1/admin/identity/participants") {
		t.Error("a non-login admin path is token-free")
	}
}

func TestUpstreamsAreTheThreeServices(t *testing.T) {
	got := Upstreams()
	want := map[string]bool{"core": true, "geodata": true, "activity": true}
	if len(got) != len(want) {
		t.Fatalf("Upstreams() = %v, want %d distinct services", got, len(want))
	}
	for _, name := range got {
		if !want[name] {
			t.Errorf("Upstreams() contains %q, which is not a service", name)
		}
	}
}

func TestTableIsNotEmpty(t *testing.T) {
	if len(Table) == 0 {
		t.Fatal("the generated route table is empty; the gateway would 404 everything")
	}
}
