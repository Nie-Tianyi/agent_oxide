//! Shell command execution for `!command` (user-initiated) invocations.
//!
//! Spawns a child process in the workspace root, captures stdout/stderr,
//! enforces a 30-second timeout via watchdog thread, and decodes output
//! respecting the system ANSI code page on Windows.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::sandbox::env_sanitizer;

/// Maximum output bytes returned — mirrors the `ShellTool` limit.
const MAX_OUTPUT_BYTES: usize = 100_000;

/// Executes a shell command in the workspace root, capturing stdout and stderr.
///
/// On Windows, uses `cmd /C` for near-instant startup (unlike PowerShell which
/// loads .NET CLR on every invocation). Encoding is handled via
/// [`decode_windows_stdout`], which tries UTF-8 first and falls back to the
/// system ANSI code page.
///
/// Environment is sanitised via [`env_sanitizer::sanitize`] before spawning,
/// and output is truncated at [`MAX_OUTPUT_BYTES`] to prevent flooding
/// the conversation context.
pub fn execute_shell_command(command: &str, workspace_root: &Path) -> String {
    #[cfg(target_os = "windows")]
    let (shell, shell_arg) = ("cmd", "/C");
    #[cfg(not(target_os = "windows"))]
    let (shell, shell_arg) = ("sh", "-c");

    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(command)
        .current_dir(workspace_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Sanitize environment before spawning (same as LLM-facing ShellTool).
    env_sanitizer::sanitize(&mut cmd, workspace_root, true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Failed to spawn command: {e}"),
    };

    let pid = child.id();

    // Watchdog: polls every 100ms, kills the process if it exceeds the
    // timeout. An AtomicBool signal lets it exit early when the command
    // completes quickly — without this, join() would block for the full
    // timeout duration even for a 15ms `dir`.
    let done = Arc::new(AtomicBool::new(false));
    let done_signal = Arc::clone(&done);

    let timeout = Duration::from_secs(30);
    let watchdog = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if done_signal.load(Ordering::Relaxed) {
                return; // command finished, no kill needed
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Timeout reached — best-effort kill.
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    });

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return format!("Failed to wait on command: {e}"),
    };

    // Signal the watchdog that the command is done, then join.
    // The watchdog checks the flag every 100ms, so join returns
    // within 100ms instead of blocking for the full 30s timeout.
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();

    let stdout = decode_stdout(&output.stdout);
    let stderr = decode_stdout(&output.stderr);
    let exit_code = output.status.code();

    let stdout_clean = stdout.trim_end();
    let stderr_clean = stderr.trim_end();

    let mut result = String::new();
    if !stdout_clean.is_empty() {
        result.push_str(&truncate_output(stdout_clean, MAX_OUTPUT_BYTES));
    }
    if !stderr_clean.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        // Reserve ~20% of budget for stderr (or at least 10KB).
        let stderr_max = (MAX_OUTPUT_BYTES / 5).max(10_240);
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(result.len());
        let stderr_limit = stderr_max.min(remaining);
        result.push_str(&truncate_output(stderr_clean, stderr_limit));
    }

    // If nothing was produced, indicate the command ran
    if result.is_empty() {
        match exit_code {
            Some(0) => result.push_str("(command completed with no output)"),
            Some(code) => {
                result.push_str(&format!("(exit code: {code}, no output)"));
            }
            None => result.push_str("(process terminated by signal, no output)"),
        }
    } else if let Some(code) = exit_code
        && code != 0
    {
        result.push_str(&format!("\n\n[exit code: {code}]"));
    }

    result
}

/// Truncate a string at `max` bytes, preserving a valid UTF-8 boundary.
fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let boundary = s.floor_char_boundary(max);
        format!("{}…\n[output truncated at {max} bytes]", &s[..boundary])
    }
}

/// Decodes child-process stdout/stderr bytes to a Rust string.
///
/// On Windows, many CLI tools (especially cmd built-ins like `dir`, `echo`,
/// and older programs) output in the system ANSI code page (e.g. GBK/CP936 for
/// Chinese-locale machines). Modern tools (git, cargo, rustc, python 3.7+)
/// typically output UTF-8 when stdout is not a TTY.
///
/// Strategy: try UTF-8 first. If every byte is valid UTF-8, use it. Otherwise
/// use the Windows [`GetACP`] code page via [`MultiByteToWideChar`]. On Unix
/// this is just [`String::from_utf8_lossy`].
#[cfg(target_os = "windows")]
fn decode_stdout(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Try UTF-8 first — modern tools output valid UTF-8.
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_string();
    }
    // Fall back to the system ANSI code page (e.g. CP936 for zh-CN).
    unsafe {
        let acp = GetACP();
        // CP 65001 IS UTF-8 — if the system already uses UTF-8, just
        // replace invalid sequences (shouldn't happen if from_utf8 failed).
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
        CodePage: u32,
        dwFlags: u32,
        lpMultiByteStr: *const i8,
        cbMultiByte: i32,
        lpWideCharStr: *mut u16,
        cchWideChar: i32,
    ) -> i32;
}

#[cfg(not(target_os = "windows"))]
fn decode_stdout(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
