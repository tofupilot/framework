//! Cross-platform path normalization helpers.
//!
//! `std::fs::canonicalize` on Windows returns an extended-length
//! `\\?\` path. `CreateProcessW` rejects that prefix as a working
//! directory under several AV/EDR products (surfacing as
//! `Access is denied (os error 5)`), and the prefix also leaks into
//! log lines / report paths shown to operators.

use std::path::PathBuf;

/// Maximum path length that `CreateProcessW` accepts without the
/// `\\?\` extended-length prefix on Windows. The kernel-side limit is
/// 32767, but most public Win32 entrypoints (including
/// `CreateProcessW` argv + cwd) refuse anything > MAX_PATH unless the
/// caller uses the extended-length form. We keep the prefix for paths
/// over this threshold so deeply-nested deployments still spawn; the
/// AV/EDR friction the strip is designed to avoid is only observed on
/// short paths anyway.
#[cfg(windows)]
const WIN_MAX_PATH: usize = 260;

/// Strip the Windows `\\?\` extended-length prefix if present, except
/// when the resulting path would exceed `MAX_PATH` — in that case the
/// prefix is the only thing letting Win32 reach the file at all, so
/// keep it. Handles both forms `canonicalize` emits:
///   - drive-letter:   `\\?\C:\path\…`         → `C:\path\…`
///   - UNC share:      `\\?\UNC\server\share\…` → `\\server\share\…`
///
/// No-op on non-Windows.
pub fn strip_unc_prefix(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            // `\\?\UNC\server\share\…` → `\\server\share\…`
            let stripped = format!(r"\\{}", rest);
            return if stripped.len() <= WIN_MAX_PATH {
                PathBuf::from(stripped)
            } else {
                p
            };
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return if stripped.len() <= WIN_MAX_PATH {
                PathBuf::from(stripped)
            } else {
                p
            };
        }
    }
    p
}

/// `std::fs::canonicalize` + UNC prefix strip. Use anywhere the
/// canonical path is handed to `CreateProcessW` (argv, cwd) or to
/// operator-visible logs.
pub fn canonicalize_for_spawn(p: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(strip_unc_prefix)
}

/// Pre-flight diagnostics for an interpreter path the caller is about
/// to `spawn`. Returns a human-readable message naming the most
/// likely cause when the OS will refuse the spawn, or `None` when
/// the interpreter looks fine. Designed so the message replaces the
/// generic `Permission denied (os error 13)` / `Access is denied
/// (os error 5)` text operators normally see.
///
/// Checks (in order):
///   - file exists
///   - on Unix: file has execute bit for the current user
///   - file is not a directory
///   - on Unix: file is not a broken symlink
pub fn diagnose_interpreter(p: &std::path::Path) -> Option<String> {
    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => {
            // metadata follows symlinks; symlink_metadata distinguishes
            // missing-target from missing-link.
            return match std::fs::symlink_metadata(p) {
                Ok(_) => Some(format!(
                    "Python interpreter {} is a symlink pointing to a non-existent target — recreate the venv.",
                    p.display()
                )),
                Err(_) => Some(format!(
                    "Python interpreter {} does not exist — recreate the venv (`uv sync` / `python -m venv .venv`).",
                    p.display()
                )),
            };
        }
    };
    if meta.is_dir() {
        return Some(format!(
            "Python interpreter path {} is a directory, not an executable.",
            p.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        // Any execute bit (owner / group / other) covers the common
        // case; the OS will pick the appropriate one. Explicit
        // owner-x check would miss venvs whose binaries are
        // group-readable in shared-dev / CI setups.
        if mode & 0o111 == 0 {
            return Some(format!(
                "Python interpreter {} is missing the execute bit (mode {:o}). Likely cause: venv was unpacked from a tar/zip without mode preservation, or rsynced without `-p`. Fix: `chmod +x {}` or recreate the venv.",
                p.display(),
                mode & 0o7777,
                p.display()
            ));
        }
    }
    None
}

/// An I/O failure as a person reads it: `not found`, `permission denied`,
/// without the `(os error N)` the platform appends.
pub fn io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => "not found".to_string(),
        ErrorKind::PermissionDenied => "permission denied".to_string(),
        ErrorKind::IsADirectory => "is a directory".to_string(),
        ErrorKind::InvalidData => "not UTF-8 text".to_string(),
        _ => {
            let text = e.to_string();
            match text.find(" (os error ") {
                Some(i) => text[..i].to_string(),
                None => text,
            }
        }
    }
}
