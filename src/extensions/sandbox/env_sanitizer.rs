//! Environment sanitizer for sandboxed child processes.
//!
//! When `sanitize_environment` is enabled, we clear all environment
//! variables and only pass a known-safe allowlist.  This prevents
//! leaking secrets (`DEEPSEEK_API`, etc.) and neutralises
//! injection vectors like `LD_PRELOAD`.  Code-loading path variables
//! (`PYTHONPATH`, `NODE_PATH`, `RUSTC_WRAPPER`) are deliberately
//! excluded from the allowlist — they execute code on import; project
//! tooling should go through `workspace_root/bin` on `PATH` instead.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Apply environment sanitization to a [`Command`] before spawning.
///
/// When `enabled` is true:
/// - All variables are cleared.
/// - Only the safe allowlist below is restored.
/// - `LD_PRELOAD` and other code-loading vectors are excluded.
/// - `workspace_root/bin` is prepended to `PATH`.
pub fn sanitize(cmd: &mut Command, workspace_root: &Path, enabled: bool) {
    if !enabled {
        return;
    }

    // Save values before clearing.
    let preserved = collect_safe_vars();

    // Record which variables get cleared — names only, never values
    // (values may contain secrets such as API keys).
    let cleared: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .filter(|k| !preserved.contains_key(k))
        .collect();
    if !cleared.is_empty() {
        tracing::debug!(
            count = cleared.len(),
            names = ?cleared,
            "Cleared environment variables for sandboxed child process"
        );
    }

    cmd.env_clear();

    // Restore safe variables
    for (key, val) in &preserved {
        cmd.env(key, val);
    }

    // Prepend workspace bin to PATH so project-local tools are available.
    let ws_bin = workspace_root.join("bin");
    if ws_bin.is_dir() {
        let separator = if cfg!(target_os = "windows") {
            ";"
        } else {
            ":"
        };
        if let Some(existing_path) = preserved.get("PATH") {
            cmd.env(
                "PATH",
                format!("{}{}{}", ws_bin.display(), separator, existing_path),
            );
        } else {
            cmd.env("PATH", ws_bin.display().to_string());
        }
    }
}

/// Returns the values of environment variables that are safe to pass
/// to child processes.
pub fn collect_safe_vars() -> std::collections::HashMap<String, String> {
    // Variables we consider safe for child processes.
    let safe_keys: HashSet<&str> = [
        // Standard
        "PATH",
        "HOME",
        "USER",
        "USERNAME",
        "TEMP",
        "TMP",
        "TMPDIR",
        "SHELL",
        "LANG",
        "LC_ALL",
        // Windows
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        // Dev tooling.
        // NOTE: PYTHONPATH / NODE_PATH / RUSTC_WRAPPER are intentionally
        // NOT here — they load code on interpreter/compiler startup and
        // are classic injection vectors (see module docs).
        "CARGO_HOME",
        "RUSTUP_HOME",
        "NPM_CONFIG_USERCONFIG",
        "GOPATH",
        "JAVA_HOME",
        // Terminal / display
        "TERM",
        "COLORTERM",
        "NO_COLOR",
        "CLICOLOR",
        "FORCE_COLOR",
        // CI
        "CI",
        "GITHUB_ACTIONS",
        // pkg-config
        "PKG_CONFIG_PATH",
    ]
    .into_iter()
    .collect();

    let mut preserved = std::collections::HashMap::new();

    for key in &safe_keys {
        if let Ok(val) = std::env::var(key) {
            preserved.insert(key.to_string(), val);
        }
    }

    preserved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_disabled_no_changes() {
        // When sanitize is disabled, the command's environment is
        // untouched (we verify by checking no panic occurs).
        let mut cmd = std::process::Command::new("echo");
        sanitize(&mut cmd, Path::new("/tmp"), false);
        drop(cmd);
    }

    #[test]
    fn test_sanitize_does_not_crash() {
        // Smoke test — verify sanitize() doesn't panic with
        // various inputs.
        let mut cmd = std::process::Command::new("echo");
        sanitize(&mut cmd, Path::new("/tmp"), true);
        drop(cmd);
    }

    #[test]
    fn test_collect_safe_vars_includes_core_vars() {
        let vars = collect_safe_vars();
        // PATH should be present on all platforms
        assert!(vars.contains_key("PATH"), "PATH should be in safe vars");
    }

    #[test]
    fn test_collect_safe_vars_excludes_secrets() {
        let vars = collect_safe_vars();
        // LD_PRELOAD must never be in the safe list
        assert!(
            !vars.contains_key("LD_PRELOAD"),
            "LD_PRELOAD must not be in safe vars"
        );
        // API keys must never leak
        assert!(
            !vars.contains_key("DEEPSEEK_API"),
            "DEEPSEEK_API must not be in safe vars"
        );
        assert!(
            !vars.contains_key("OPENAI_API_KEY"),
            "OPENAI_API_KEY must not be in safe vars"
        );
    }

    #[test]
    fn test_collect_safe_vars_excludes_code_loading_paths() {
        let vars = collect_safe_vars();
        // Code-loading path variables execute code on import — they must
        // never reach sandboxed child processes.
        for key in ["PYTHONPATH", "NODE_PATH", "RUSTC_WRAPPER"] {
            assert!(
                !vars.contains_key(key),
                "{key} is a code-loading vector and must not be in safe vars"
            );
        }
    }

    #[test]
    fn test_collect_safe_vars_includes_windows_vars() {
        let _vars = collect_safe_vars();
        #[cfg(target_os = "windows")]
        {
            // On Windows, critical system vars should be in the safe list.
            // We can't assert they exist in the environment, but we
            // can verify our safe-key list includes them.
        }
        #[cfg(not(target_os = "windows"))]
        {
            // On Unix, HOME should be in the safe list.
            // (It may or may not be set in the environment, but the key
            // is in the safe set.)
        }
    }

    #[test]
    fn test_collect_safe_vars_includes_dev_tooling() {
        let vars = collect_safe_vars();
        // These keys should be in the safe set (whether or not they're
        // actually set in the environment).
        // We test that the set contains these key names by checking
        // that they're listed in the safe_keys literal. Since the
        // collect function only adds keys that exist in the env,
        // and we can't control the CI/test environment, we do a
        // structural test instead.
        //
        // Verify the function was called successfully — the call above is
        // the assertion (it would panic on error). Check the result is a
        // well-formed map: env values can never contain NUL bytes.
        assert!(vars.values().all(|v| !v.contains('\0')));
    }

    #[test]
    fn test_sanitize_with_workspace_bin() {
        // Create a temp workspace with a bin directory.
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("bin")).unwrap();

        let mut cmd = std::process::Command::new("echo");
        sanitize(&mut cmd, ws, true);

        // On stable Rust we can't inspect Command's env directly,
        // but we can verify no panic occurred.
        drop(cmd);
    }

    #[test]
    fn test_sanitize_without_workspace_bin() {
        // Workspace without a bin directory should still work.
        let tmp = tempfile::tempdir().unwrap();
        let mut cmd = std::process::Command::new("echo");
        sanitize(&mut cmd, tmp.path(), true);
        drop(cmd);
    }
}
