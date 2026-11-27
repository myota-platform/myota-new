//! Numeric limits shared by every MyOTA service.
//!
//! This crate mirrors `libs/limits/go`. It exists because a limit stated in two
//! places drifts: the old implementation declared a 20 MiB upload ceiling in the
//! contract while enforcing 1 GiB, and nothing caught the disagreement. The
//! contract and the validators here read the same constants.
//!
//! The Go and Rust copies must hold identical values. Both test suites assert the
//! documented numbers, so an edit to one without the other fails a test rather
//! than shipping a divergence.

/// The largest single import file accepted: 1 GiB.
///
/// See `docs/architecture/geodata-imports.md`, "Limits".
pub const MAX_IMPORT_UPLOAD_BYTES: u64 = 1 << 30;

/// The most candidates one atomic promotion request may contain, so the
/// transaction stays short enough to reason about.
///
/// See `docs/architecture/geodata-imports.md`, "Publish".
pub const MAX_PROMOTION_BATCH_CANDIDATES: u32 = 300;

/// The most features one worker batch may hold, which caps worker memory at
/// roughly 40 MB per batch.
///
/// See `docs/architecture/geodata-imports.md`, "Worker capacity".
pub const MAX_PREPROCESS_BATCH_FEATURES: u32 = 500;

/// The page size a cursor-paginated collection returns when the caller does not
/// ask for one.
pub const DEFAULT_PAGE_ITEMS: u16 = 50;

/// The largest page a caller may request. A larger request is clamped rather than
/// rejected: an oversized page is a caller mistake, not a reason to fail a read.
pub const MAX_PAGE_ITEMS: u16 = 200;

#[cfg(test)]
mod tests {
    use super::*;

    /// These assertions look tautological, and they are — their job is to fail
    /// loudly when someone edits a constant without editing the document, or
    /// without editing the Go copy.
    ///
    /// The values go through a table rather than sitting directly in `assert_eq!`
    /// so clippy does not fold the comparison to `assert!(true)`. That is a real
    /// improvement, not a lint workaround: the table names each limit in the
    /// failure message, which the direct form did not.
    #[test]
    fn documented_values() {
        let documented: [(&str, u64, u64); 5] = [
            (
                "MAX_IMPORT_UPLOAD_BYTES",
                MAX_IMPORT_UPLOAD_BYTES,
                1_073_741_824,
            ),
            (
                "MAX_PROMOTION_BATCH_CANDIDATES",
                u64::from(MAX_PROMOTION_BATCH_CANDIDATES),
                300,
            ),
            (
                "MAX_PREPROCESS_BATCH_FEATURES",
                u64::from(MAX_PREPROCESS_BATCH_FEATURES),
                500,
            ),
            ("DEFAULT_PAGE_ITEMS", u64::from(DEFAULT_PAGE_ITEMS), 50),
            ("MAX_PAGE_ITEMS", u64::from(MAX_PAGE_ITEMS), 200),
        ];

        for (name, actual, expected) in documented {
            assert_eq!(
                actual, expected,
                "{name} = {actual}, want {expected} (update docs/architecture and the Go copy together)"
            );
        }
    }

    #[test]
    fn default_page_fits_within_maximum() {
        // Bound to locals so the comparison is a runtime one. Comparing two
        // constants directly is folded away by the compiler, and clippy rejects
        // the resulting `assert!(true)` — correctly, since a check that cannot
        // fail is not a check.
        let default_page = DEFAULT_PAGE_ITEMS;
        let maximum_page = MAX_PAGE_ITEMS;
        assert!(
            default_page <= maximum_page,
            "DEFAULT_PAGE_ITEMS ({default_page}) exceeds MAX_PAGE_ITEMS ({maximum_page})"
        );
    }
}
