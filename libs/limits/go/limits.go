// Package limits is the single place numeric limits are defined.
//
// It exists because a limit stated in two places drifts. The upload ceiling was
// declared as 20 MiB in the old OpenAPI document while the code enforced 1 GiB;
// the contract and the validator disagreed and nothing caught it. Here the
// contract generator and the validators import the same constants, so they
// cannot disagree.
//
// Values are recorded with the document they come from. When one changes, the
// document changes with it.
package limits

const (
	// MaxImportUploadBytes is the largest single import file accepted.
	// See docs/architecture/geodata-imports.md, "Limits".
	MaxImportUploadBytes int64 = 1 << 30 // 1 GiB

	// MaxPromotionBatchCandidates bounds one atomic promotion request, so the
	// transaction stays short enough to reason about.
	// See docs/architecture/geodata-imports.md, "Publish".
	MaxPromotionBatchCandidates = 300

	// MaxPreprocessBatchFeatures bounds one worker batch, which caps worker
	// memory at roughly 40 MB per batch.
	// See docs/architecture/geodata-imports.md, "Worker capacity".
	MaxPreprocessBatchFeatures = 500

	// DefaultPageItems is the page size a cursor-paginated collection returns
	// when the caller does not ask for one.
	DefaultPageItems = 50

	// MaxPageItems is the largest page a caller may request. A request above it
	// is clamped, not rejected: an oversized page is a caller mistake, not an
	// error worth failing a read for.
	MaxPageItems = 200
)
