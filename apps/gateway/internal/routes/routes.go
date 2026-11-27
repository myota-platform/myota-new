// Package routes holds the prefix table the gateway proxies on.
//
// The table is generated from the contract (`table_gen.go`) so that the gateway
// cannot route a prefix the contract does not declare, and cannot fail to route one
// it does. Everything in this file is the lookup logic around that data.
package routes

import (
	"strings"
)

// Audience is the credential class a prefix accepts.
type Audience string

const (
	// AudienceAdmin means the request must carry an administrator token or none at
	// all, and none only on the paths the contract marks token-free. A participant
	// credential is refused with 401 `about:blank#wrong-audience`.
	AudienceAdmin Audience = "admin"
	// AudienceOpen means the edge does not judge the audience: the contract offers
	// the prefix to anonymous callers, or to both audiences, and the service decides
	// what a given caller may do.
	AudienceOpen Audience = "open"
)

// Route is one prefix and what happens to a request that matches it.
type Route struct {
	// Prefix is a two-segment path with a trailing slash, e.g. "/v1/entities/", or
	// three segments under /v1/admin/.
	Prefix string
	// Upstream is the service name, matching a configured base URL: "core",
	// "geodata" or "activity".
	Upstream string
	// TokenFree lists exact paths under this prefix that the contract marks
	// `security: []`. On an AudienceAdmin prefix these are reachable without a
	// token — a login form cannot require a login.
	TokenFree []string
	// Audience is the credential class the prefix accepts.
	Audience Audience
}

// Lookup returns the longest prefix that path falls under.
//
// Longest, not first, because prefixes can nest: `/v1/admin/auth/` sits beside
// `/v1/admin/awards/`, and a later `/v1/admin/` catch-all must not shadow either. A
// linear scan over a table this size is the clearest correct implementation; a trie
// would be faster and harder to verify, which is the wrong trade for 22 entries.
func Lookup(path string) (Route, bool) {
	var best Route
	found := false
	for _, route := range Table {
		// The table stores prefixes with a trailing slash so a prefix match cannot
		// straddle a segment boundary — `/v1/entitiesX` must not match `/v1/entities/`.
		// The one exception is the collection endpoint itself, which the contract
		// serves at the stem: `/v1/entities`, not `/v1/entities/`.
		if !strings.HasPrefix(path, route.Prefix) && path != strings.TrimSuffix(route.Prefix, "/") {
			continue
		}
		if !found || len(route.Prefix) > len(best.Prefix) {
			best = route
			found = true
		}
	}
	return best, found
}

// AllowsWithoutToken reports whether path may be reached with no credential on an
// administrator prefix. It is an exact match against the generated list, never a
// pattern: `/v1/admin/auth/login` is token-free and `/v1/admin/auth/refresh` is not,
// and a prefix match would collapse that distinction.
func (r Route) AllowsWithoutToken(path string) bool {
	for _, candidate := range r.TokenFree {
		if candidate == path {
			return true
		}
	}
	return false
}

// Upstreams returns the distinct upstream names in the table, in table order. The
// gateway needs the list at startup to check that every one has a configured base
// URL — a missing URL would otherwise be a 500 on the first request to that prefix.
func Upstreams() []string {
	seen := make(map[string]struct{}, 3)
	names := make([]string, 0, 3)
	for _, route := range Table {
		if _, ok := seen[route.Upstream]; ok {
			continue
		}
		seen[route.Upstream] = struct{}{}
		names = append(names, route.Upstream)
	}
	return names
}
