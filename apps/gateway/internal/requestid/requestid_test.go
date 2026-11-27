package requestid

import (
	"context"
	"testing"
)

func TestGenerateProducesVersion4UUIDs(t *testing.T) {
	seen := make(map[string]struct{}, 1000)
	for range 1000 {
		id := Generate()
		if !wellFormed.MatchString(id) {
			t.Fatalf("Generate() = %q, which is not UUID-shaped", id)
		}
		if id[14] != '4' {
			t.Fatalf("Generate() = %q, version nibble is %q, want 4", id, id[14])
		}
		if _, duplicate := seen[id]; duplicate {
			t.Fatalf("Generate() repeated %q within 1000 draws", id)
		}
		seen[id] = struct{}{}
	}
}

func TestFromHeader(t *testing.T) {
	cases := []struct {
		name   string
		header string
		keep   bool
	}{
		{"a caller's well-formed id is preserved", "9F2C1E00-0000-4000-8000-000000000001", true},
		{"lowercase is preserved", "9f2c1e00-0000-4000-8000-000000000001", true},
		{"empty gets a fresh id", "", false},
		// Log injection: a newline in a header would forge a second log line.
		{"a newline is refused", "9f2c1e00-0000-4000-8000-000000000001\nevil", false},
		{"a short id is refused", "abc", false},
		{"a trailing injection is refused", "9f2c1e00-0000-4000-8000-000000000001 extra", false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got := FromHeader(c.header)
			if c.keep {
				if got != c.header {
					t.Errorf("FromHeader(%q) = %q, want it preserved", c.header, got)
				}
				return
			}
			if got == c.header {
				t.Errorf("FromHeader(%q) returned the input, want a generated id", c.header)
			}
			if !wellFormed.MatchString(got) {
				t.Errorf("FromHeader(%q) = %q, want a generated UUID", c.header, got)
			}
		})
	}
}

func TestFromContext(t *testing.T) {
	if got := FromContext(context.Background()); got != "" {
		t.Errorf("FromContext(empty) = %q, want the empty string", got)
	}
	ctx := NewContext(context.Background(), "9f2c1e00-0000-4000-8000-000000000001")
	if got := FromContext(ctx); got != "9f2c1e00-0000-4000-8000-000000000001" {
		t.Errorf("FromContext = %q, want the stored id", got)
	}
}
