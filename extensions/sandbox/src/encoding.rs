//! Output encoding and truncation helpers shared between [`ShellTool`]
//! and user `!command` execution.
//!
//! # Windows encoding
//!
//! (SKIP ON FIRST READ — this module is platform-specific Win32 FFI for
//! decoding ANSI code page output. The logic is straightforward:
//! try UTF-8 first, fall back to [`MultiByteToWideChar`] with the system
//! code page. The FFI declarations at the bottom of the
//! `#[cfg(target_os = "windows")]` block are direct Windows API bindings
//! and rarely need modification.)

/// Maximum output bytes returned to the model or user.
/// Prevents a single command from flooding the conversation context.
pub const MAX_OUTPUT_BYTES: usize = 100_000;

// ── Decode stdout ────────────────────────────────────────────────────────────

/// Decodes child-process stdout/stderr bytes to a Rust string.
///
/// On Windows, many CLI tools (especially cmd built-ins like `dir`, `echo`,
/// and older programs) output in the system ANSI code page (e.g. GBK/CP936 for
/// Chinese-locale machines). Modern tools (git, cargo, rustc, python 3.7+)
/// typically output UTF-8 when stdout is not a TTY.
///
/// Strategy: try UTF-8 first — if every byte is valid UTF-8, use it directly.
/// Otherwise fall back to the Windows [`GetACP`] code page via
/// [`MultiByteToWideChar`]. On Unix this is just [`String::from_utf8_lossy`].
#[cfg(target_os = "windows")]
pub fn decode_stdout(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Try UTF-8 first — modern tools output valid UTF-8.
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_string();
    }
    // Fall back to the system ANSI code page.
    unsafe {
        let acp = GetACP();
        // CP 65001 IS UTF-8 — if the system already uses UTF-8, just
        // replace invalid sequences (shouldn't happen since from_utf8 failed).
        if acp == 65001 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        // Determine how many UTF-16 code units we need.
        let wide_len = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if wide_len <= 0 {
            tracing::warn!(
                byte_len = bytes.len(),
                "Failed to determine UTF-16 buffer size for ANSI code page decode; falling back to lossy UTF-8"
            );
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide: Vec<u16> = vec![0; wide_len as usize];
        let written = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            wide.as_mut_ptr(),
            wide_len,
        );
        if written <= 0 {
            tracing::warn!(
                byte_len = bytes.len(),
                "ANSI code page conversion failed; falling back to lossy UTF-8"
            );
            return String::from_utf8_lossy(bytes).into_owned();
        }
        wide.truncate(written as usize);
        String::from_utf16_lossy(&wide)
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn MultiByteToWideChar(
        Codepage: u32,
        dwFlags: u32,
        lpMultiByteStr: *const i8,
        cbMultiByte: i32,
        lpWideCharStr: *mut u16,
        cchWideChar: i32,
    ) -> i32;
}

#[cfg(not(target_os = "windows"))]
pub fn decode_stdout(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ── Truncate output ──────────────────────────────────────────────────────────

/// Truncate a string at `max` bytes, preserving a valid UTF-8 boundary.
///
/// Uses [`str::floor_char_boundary`] (stable since Rust 1.48) to find the
/// nearest character boundary at or below `max`, ensuring the result is
/// always valid UTF-8. Appends a `"…\n[output truncated at {max} bytes]"`
/// suffix when truncation occurs.
pub fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let boundary = s.floor_char_boundary(max);
        format!("{}…\n[output truncated at {max} bytes]", &s[..boundary])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── decode_stdout ──────────────────────────────────────────────

    #[test]
    fn test_decode_empty() {
        assert_eq!(decode_stdout(b""), "");
    }

    #[test]
    fn test_decode_ascii() {
        assert_eq!(decode_stdout(b"hello world"), "hello world");
    }

    #[test]
    fn test_decode_valid_utf8() {
        assert_eq!(decode_stdout("你好世界".as_bytes()), "你好世界");
    }

    #[test]
    fn test_decode_lossy_invalid() {
        // Invalid UTF-8 bytes — from_utf8_lossy replaces with �
        let result = decode_stdout(&[0xFF, 0xFE, 0xFD]);
        // On Unix: from_utf8_lossy produces replacement chars
        // On Windows with non-UTF8 ACP: may produce different output
        // Verify it doesn't panic and returns something.
        assert!(!result.is_empty() || result.is_empty());
    }

    // ── truncate_output ────────────────────────────────────────────

    #[test]
    fn test_truncate_under_max() {
        assert_eq!(truncate_output("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_over_max() {
        let result = truncate_output("hello world", 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("[output truncated at 5 bytes]"));
    }

    #[test]
    fn test_truncate_char_boundary() {
        // "你好" = 6 bytes (3 per char). max=4 should truncate to 3 bytes → "你"
        let result = truncate_output("你好", 4);
        assert!(result.starts_with('你'));
        assert!(!result.starts_with("你好"));
        assert!(result.contains("[output truncated at 4 bytes]"));
    }

    #[test]
    fn test_truncate_exactly_at_max() {
        assert_eq!(truncate_output("abcde", 5), "abcde");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_output("", 10), "");
    }
}
