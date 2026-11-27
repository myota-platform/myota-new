package limits

import "testing"

// These assertions look tautological, and they are — their job is to fail loudly
// when someone edits a constant without editing the document, or the other
// language's copy. libs/limits/rust asserts the same numbers.
func TestDocumentedValues(t *testing.T) {
	cases := []struct {
		name string
		got  int64
		want int64
	}{
		{"MaxImportUploadBytes", MaxImportUploadBytes, 1 << 30},
		{"MaxPromotionBatchCandidates", MaxPromotionBatchCandidates, 300},
		{"MaxPreprocessBatchFeatures", MaxPreprocessBatchFeatures, 500},
		{"DefaultPageItems", DefaultPageItems, 50},
		{"MaxPageItems", MaxPageItems, 200},
	}
	for _, c := range cases {
		if c.got != c.want {
			t.Errorf("%s = %d, want %d (update docs/architecture and the Rust copy together)", c.name, c.got, c.want)
		}
	}
}

func TestDefaultPageFitsWithinMaximum(t *testing.T) {
	if DefaultPageItems > MaxPageItems {
		t.Fatalf("DefaultPageItems (%d) exceeds MaxPageItems (%d)", DefaultPageItems, MaxPageItems)
	}
}
