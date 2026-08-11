//! # Utilities
//!
//! Small helpers shared across the workspace (persistence crate, sandbox
//! audit logging, hooks in the binary).

use time::OffsetDateTime;
use time::macros::format_description;

/// Returns the current UTC time as an ISO-8601 formatted string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Second-precision to keep the output stable across call sites (thread
/// filenames, `saved_at` markers). Deliberately not [`time::format_description::well_known::Rfc3339`],
/// which would append fractional seconds.
pub fn iso8601_now() -> String {
    OffsetDateTime::now_utc()
        .format(&format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .expect("compile-time-validated format")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso8601_now_produces_correct_format() {
        let ts = iso8601_now();
        // Should look like "2026-07-09T12:34:56Z"
        assert!(ts.ends_with('Z'), "got {ts}");
        assert_eq!(ts.len(), 20, "got {ts}");
        assert!(ts.starts_with("20"), "got {ts}");
        let parts: Vec<&str> = ts[..19].split('T').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 10);
        assert_eq!(parts[1].len(), 8);
    }
}
