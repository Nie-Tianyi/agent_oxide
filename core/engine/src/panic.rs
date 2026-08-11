//! Panic-payload helpers shared by the panic hook and task protection code.

use std::any::Any;

/// Extract a human-readable message from a panic payload.
///
/// Rust panics carry either a `&str` or a `String` as their payload; anything
/// else (e.g. `panic_any`) is reported generically. The default panic hook
/// uses this same downcast logic internally.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_str_payload() {
        let msg = std::panic::catch_unwind(|| panic!("boom")).unwrap_err();
        assert_eq!(panic_message(msg.as_ref()), "boom");
    }

    #[test]
    fn test_string_payload() {
        let msg = std::panic::catch_unwind(|| {
            std::panic::panic_any(String::from("boom with string"));
        })
        .unwrap_err();
        assert_eq!(panic_message(msg.as_ref()), "boom with string");
    }

    #[test]
    fn test_unknown_payload() {
        let msg = std::panic::catch_unwind(|| std::panic::panic_any(42u32)).unwrap_err();
        assert_eq!(panic_message(msg.as_ref()), "unknown panic");
    }
}
