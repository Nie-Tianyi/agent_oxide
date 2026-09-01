//! # Utilities
//!
//! Small helpers shared across the workspace — core and extensions alike.

pub(crate) mod md;

use crate::provider::{Message, Role};
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

/// Insert `msg` after the trailing System-message block — i.e. before the
/// first non-System message, or at the end if there are none.
///
/// Injector hooks (compaction, skills, context blocks) must use this instead
/// of `insert(0, …)`: volatile injected content (`[SKILL]`, `[PLAN_MODE]`,
/// `[PROFILE]`, `[COMPACT_SUMMARY]`, `[TODO]`) placed in front of the static
/// system prompt would invalidate the provider's prompt-cache prefix on
/// every change, forcing a full re-parse of the most expensive (and most
/// stable) part of the request.  Anchoring the insert at the tail of the
/// System block keeps the static head byte-identical, so only the dynamic
/// history misses.
///
/// The hook registration order decides the ordering inside the tail: the
/// more volatile an injector, the later it must run, so it lands closer to
/// the history.  `TodoListHook` is the most volatile ([TODO] changes on
/// every plan update) and runs last, keeping the rarely-changing
/// `[COMPACT_SUMMARY]` before it — a plan update then only invalidates the
/// cache prefix past the history boundary, never the compaction summary.
pub fn insert_before_history(messages: &mut Vec<Message>, msg: Message) {
    let idx = messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len());
    messages.insert(idx, msg);
}

/// Returns the largest `char`-boundary index of `s` that is `<= max`.
///
/// Equivalent to [`str::floor_char_boundary`], which is only stable since
/// Rust 1.91 — this implementation keeps the workspace MSRV at 1.85.
/// Panics if `max > s.len()`.
pub fn floor_char_boundary(s: &str, max: usize) -> usize {
    assert!(
        max <= s.len(),
        "index {max} out of bounds for len {}",
        s.len()
    );
    let mut boundary = max;
    while !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_floor_char_boundary_ascii_and_utf8() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, s.len()), s.len());

        // "你好" = 6 bytes (3 per char): indices 0, 3, 6 are boundaries,
        // 1–2 and 4–5 fall mid-character.
        let s = "你好world";
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 0);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 4), 3);
        assert_eq!(floor_char_boundary(s, 5), 3);
        assert_eq!(floor_char_boundary(s, 6), 6); // 'w' starts at 6
    }

    #[test]
    fn test_floor_char_boundary_max_eq_len() {
        let s = "abc你";
        assert_eq!(floor_char_boundary(s, s.len()), s.len());
    }

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
