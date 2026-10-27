//! Python resolution for the execution engine.
//!
//! Resolves a Python executable by checking the project's venv first,
//! then falling back to system Python via `uv python find`.

use std::path::{Path, PathBuf};

/// Either return the pre-resolved interpreter (CLI's deterministic path)
/// or fall back to walk-up resolution. Centralizes the
/// `match python_path { Some => use, None => walk }` shape that
/// otherwise lived in 3 places (worker start, orchestrator init,
/// plug service spawn).
///
/// The walk path canonicalizes `fallback_dir` before traversing so a
/// relative path with `..` segments doesn't silently break
/// `Path::parent()`. The pre-resolved path is taken at face value —
/// the caller is responsible for handing in a path that already exists.
pub async fn resolve_or_walk(
    python_path: &Option<PathBuf>,
    fallback_dir: &Path,
) -> Result<String, String> {
    match python_path {
        Some(p) => Ok(p.to_string_lossy().into_owned()),
        None => {
            // Strip Windows `\\?\` prefix so the resolved interpreter
            // path doesn't leak into argv / cwd anywhere downstream.
            let abs = crate::path_utils::canonicalize_for_spawn(fallback_dir)
                .map_err(|e| format!("Cannot resolve {}: {}", fallback_dir.display(), e))?;
            resolve_python(&abs).await
        }
    }
}

/// Resolve the Python executable for a project directory.
///
/// Resolution order:
/// 1. Project venv at `<dir>/{venv,.venv}/bin/python`, walking up
///    parent directories until found (`venv/` for pulled deployments,
///    `.venv/` for local dev). The walk-up handles uv-workspace bundles
///    where the procedure lives at `<deployment>/<root_directory>/`
///    but the venv is created once at `<deployment>/venv/`.
/// 2. `uv python find` (if uv is available on PATH)
/// 3. Error -- no suitable Python found
pub async fn resolve_python(project_dir: &Path) -> Result<String, String> {
    // 1. Walk up looking for a venv. `pull/sync.rs` writes deployments
    // under `venv/` at the workspace root, while `procedure_dir` may
    // be a package subdirectory (`stations/boot_test`). Local dev
    // (uv / poetry / hatch / plain python) tends to use `.venv/`.
    // Bounded walk: 8 levels is more than any plausible monorepo depth.
    let mut current: Option<&Path> = Some(project_dir);
    for _ in 0..8 {
        let Some(dir) = current else { break };
        for name in ["venv", ".venv"] {
            let unix = dir.join(name).join("bin/python");
            if unix.exists() {
                return Ok(unix.to_string_lossy().to_string());
            }
            let win = dir.join(name).join("Scripts/python.exe");
            if win.exists() {
                return Ok(win.to_string_lossy().to_string());
            }
        }
        current = dir.parent();
    }

    // 2. Try `uv python find`
    match tokio::process::Command::new("uv")
        .args(["python", "find"])
        .current_dir(project_dir)
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let python = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !python.is_empty() {
                return Ok(python);
            }
        }
        _ => {}
    }

    Err(
        "No Python executable found. Create a venv in the project directory or install uv."
            .to_string(),
    )
}

pub fn to_python_identifier(s: &str) -> String {
    let transliterate = |c: char| -> char {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            'ñ' => 'n',
            'ç' => 'c',
            'æ' => 'a',
            'œ' => 'o',
            _ => c,
        }
    };

    let mut result: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(transliterate)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else if c.is_whitespace() || c == '-' {
                '_'
            } else {
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if result.is_empty() {
        return result;
    }

    if result.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        result.insert(0, '_');
    }

    result
}

pub fn is_valid_python_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();

    if let Some(first) = chars.next() {
        if !first.is_ascii_alphabetic() && first != '_' {
            return false;
        }
    }

    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
