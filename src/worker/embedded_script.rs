//! Resolution + extraction for the embedded `tp_worker.py` / `tp_plug.py`
//! Python scripts the engine spawns.
//!
//! Resolution order:
//! 1. `<exe_dir>/python/<name>` — Studio/packaged layout that ships the
//!    script next to the binary.
//! 2. `<runtime_dir>/<name>` — extracted from the embedded constant on
//!    first use. Lives under `~/.tofupilot/runtime/`, NOT `%TEMP%` —
//!    Windows Defender ASR rules ("Block executable content from
//!    email/webmail" et al.) and several enterprise EDR policies block
//!    script execution from `AppData\Local\Temp`, surfacing as
//!    `Access is denied (os error 5)` on the first spawn. A per-user,
//!    persistent location lets AV scan once and stay quiet. Sharing
//!    `~/.tofupilot/` with the rest of CLI state also means uninstall
//!    sweeps the runtime dir along with creds / redb in one shot.
//!
//! The extracted file is named by its content hash, so CLI versions
//! sharing the runtime dir never overwrite each other's copy.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Subdirectory under `~/.tofupilot/` for extracted helper scripts.
/// Public so the CLI's uninstaller can target it without re-deriving
/// the layout.
pub const RUNTIME_SUBDIR: &str = "runtime";

fn runtime_dir() -> PathBuf {
    // Engine can't depend on CLI, so we resolve `~/.tofupilot/runtime`
    // here, honouring `TOFUPILOT_HOME` (an absolute path) as the CLI's
    // `db::home_dir` does for every other file. Fallback to
    // `std::env::temp_dir()/tofupilot-fallback` only when no home dir
    // resolves (CI, sandboxes); in that pathological case AV friction is
    // unavoidable.
    let home = std::env::var_os("TOFUPILOT_HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .or_else(dirs::home_dir);
    let base = home
        .map(|h| h.join(".tofupilot"))
        .unwrap_or_else(|| std::env::temp_dir().join("tofupilot-fallback"));
    base.join(RUNTIME_SUBDIR)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for byte in out.iter() {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", byte);
    }
    s
}

/// Resolve `<exe_dir>/python/<name>` if it exists.
pub fn next_to_exe(name: &str) -> Option<PathBuf> {
    let exe_path = std::env::current_exe().ok()?;
    let exe_dir = exe_path.parent()?;
    let script_path = exe_dir.join("python").join(name);
    if script_path.exists() {
        Some(script_path)
    } else {
        None
    }
}

/// Extract `script_bytes` to `<runtime_dir>/<stem>.<hash8>.<ext>` if not
/// already present with matching content. Returns the script path.
///
/// The name carries the content hash because several CLI versions share
/// one runtime dir (a station daemon and a developer's CLI on the same
/// machine): with one fixed name, each rewrote the other's copy and a
/// process spawned the other version's script, whose protocol it does
/// not speak. A hashed name is written once and never changes.
///
/// Concurrent callers may race on the first write; the worst case is
/// two processes writing identical bytes. The first copy is installed
/// with a hard link, which never replaces a file, so a reader never sees
/// a good copy swapped out under it. Only a corrupt copy is renamed
/// over. On Windows, `rename` over a file opened without
/// `FILE_SHARE_DELETE` returns `os error 5`; if the rename fails but the
/// existing file already has the expected hash, treat it as a
/// win-by-another-process and return success.
pub fn extract_to_runtime_dir(name: &str, script_bytes: &str) -> Result<PathBuf, String> {
    let dir = runtime_dir();
    let path = extract_into(&dir, name, script_bytes)?;
    prune_once(&dir, name, &path);
    Ok(path)
}

fn hashed_name(name: &str, expected: &str) -> String {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    format!("{stem}.{}.{ext}", &expected[..HASH_CHARS])
}

/// Hex characters of the content hash kept in the file name.
const HASH_CHARS: usize = 8;

fn extract_into(dir: &std::path::Path, name: &str, script_bytes: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {}", dir.display(), e))?;
    let expected = hex_digest(script_bytes.as_bytes());
    let script_path = dir.join(hashed_name(name, &expected));

    if hash_matches(&script_path, &expected) {
        touch(&script_path);
        return Ok(script_path);
    }

    // Write to a temp neighbor then rename. The name is unique per call,
    // not per process: workers start in parallel threads, and with one
    // name per pid a thread truncated the file another had just renamed
    // into place, or found its own copy already renamed away (ENOENT).
    // Random rather than a counter: containers sharing one home reuse pids.
    // The pid stays first for the prune.
    let tmp = script_path.with_extension(format!(
        "py.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp, script_bytes.as_bytes())
        .map_err(|e| format!("Cannot write {}: {}", tmp.display(), e))?;

    // Install without replacing: a hard link fails if the name exists.
    // Renaming over a good copy that another thread just installed is not
    // harmless on Windows: the replaced file turns delete-pending, and a
    // process opening it at that instant (python, or a sibling's hash
    // check) gets `Access is denied (os error 5)`.
    match std::fs::hard_link(&tmp, &script_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            return Ok(script_path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if hash_matches(&script_path, &expected) {
                let _ = std::fs::remove_file(&tmp);
                touch(&script_path);
                return Ok(script_path);
            }
            // A corrupt copy: replace it below.
        }
        // No hard links on this filesystem (FAT, some shares): rename.
        Err(_) => {}
    }
    match std::fs::rename(&tmp, &script_path) {
        Ok(()) => Ok(script_path),
        Err(e) => {
            // Another process may have installed an identical file
            // first and is now holding it open (Windows: no
            // FILE_SHARE_DELETE on the python read). If the on-disk
            // bytes match what we'd have written, we're done.
            let _ = std::fs::remove_file(&tmp);
            if hash_matches(&script_path, &expected) {
                touch(&script_path);
                Ok(script_path)
            } else {
                Err(format!("Cannot write {}: {}", script_path.display(), e))
            }
        }
    }
}

/// Mark a copy as in use, so another version's prune leaves it alone
/// for `STALE_SCRIPT_AGE` after this spawn. Best effort.
fn touch(path: &std::path::Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Gate pruning behind a per-name once so a long-running station that
/// spawns many workers / plug services doesn't `read_dir` the runtime
/// layout on every extract call. Cleanup is opportunistic — once per
/// process and script is enough to keep the layout tidy.
fn prune_once(dir: &std::path::Path, name: &str, current: &std::path::Path) {
    static PRUNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    {
        let Ok(mut pruned) = PRUNED.lock() else {
            return;
        };
        if pruned.iter().any(|n| n == name) {
            return;
        }
        pruned.push(name.to_string());
    }
    prune_stale_tmps(dir);
    prune_stale_copies(dir, name, current, STALE_SCRIPT_AGE);
}

fn hash_matches(path: &std::path::Path, expected: &str) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => hex_digest(&bytes) == expected,
        Err(_) => false,
    }
}

/// A temp copy lives for one write + rename. One older than this was
/// left by an extract that crashed; a younger one may belong to another
/// process writing it right now.
const STALE_TMP_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// Every extract touches its copy, so one untouched for this long
/// belongs to a version no process has spawned in a day. A process of
/// that version that comes back rewrites it.
const STALE_SCRIPT_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Best-effort cleanup of orphaned `*.py.tmp.<pid>.<uuid>` files left behind
/// when a previous extract crashed mid-write. Goes by age, not by pid:
/// any other pid was deleted before, a concurrent CLI's copy included,
/// which then failed its rename.
fn prune_stale_tmps(dir: &std::path::Path) {
    prune_stale_tmps_older_than(dir, STALE_TMP_AGE);
}

fn is_older_than(entry: &std::fs::DirEntry, max_age: std::time::Duration) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= max_age)
}

fn prune_stale_tmps_older_than(dir: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        // Match `<anything>.py.tmp.<pid>.<uuid>`, and the `<anything>.py.tmp.<pid>`
        // of older CLIs.
        let Some((_, suffix)) = name_str.rsplit_once(".tmp.") else {
            continue;
        };
        let pid_str = suffix.split_once('.').map_or(suffix, |(pid, _)| pid);
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if is_older_than(&entry, max_age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Remove other versions' hashed copies of `name` (`<stem>.<hash8>.<ext>`)
/// that no process touched for `max_age`. The un-hashed `<name>` of
/// older CLIs is left alone: they still rewrite and spawn it.
fn prune_stale_copies(
    dir: &std::path::Path,
    name: &str,
    current: &std::path::Path,
    max_age: std::time::Duration,
) {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(hash) = file_name
            .strip_prefix(stem)
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.strip_suffix(ext))
            .and_then(|rest| rest.strip_suffix('.'))
        else {
            continue;
        };
        let is_hash = hash.len() == HASH_CHARS && hash.bytes().all(|b| b.is_ascii_hexdigit());
        if !is_hash || entry.path() == current {
            continue;
        }
        if is_older_than(&entry, max_age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_a_fresh_tmp_of_another_process() {
        let dir = std::env::temp_dir().join(format!("tp-prune-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join(format!("tp_worker.py.tmp.{}", std::process::id() + 1));
        std::fs::write(&other, b"mid-write").unwrap();

        prune_stale_tmps(&dir);
        assert!(other.exists(), "a concurrent extract's copy was deleted");

        prune_stale_tmps_older_than(&dir, std::time::Duration::ZERO);
        assert!(!other.exists(), "an orphaned copy is still pruned");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_reads_the_pid_of_a_per_call_tmp() {
        let dir = std::env::temp_dir().join(format!("tp-prune-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join(format!("tp_worker.py.tmp.{}.3", std::process::id()));
        let orphan = dir.join(format!("tp_worker.py.tmp.{}.0", std::process::id() + 1));
        std::fs::write(&ours, b"mid-write").unwrap();
        std::fs::write(&orphan, b"crashed").unwrap();

        prune_stale_tmps_older_than(&dir, std::time::Duration::ZERO);
        assert!(ours.exists(), "this process's own copy was deleted");
        assert!(!orphan.exists(), "an orphaned per-call copy is not pruned");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_first_extracts_in_one_process_all_succeed() {
        // Workers start in parallel threads on a fresh runtime dir; every
        // one must get a complete copy.
        let script = "# worker\n".repeat(20_000);
        for _ in 0..100 {
            let dir = temp_dir();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (dir, barrier, script) = (dir.clone(), barrier.clone(), script.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        extract_into(&dir, "tp_worker.py", &script)
                    })
                })
                .collect();
            for handle in handles {
                let path = handle.join().unwrap().unwrap();
                assert_eq!(std::fs::read_to_string(&path).unwrap(), script);
            }
            let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
            assert_eq!(left.len(), 1, "temp copies left behind: {left:?}");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tp-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn two_versions_get_two_paths_and_keep_their_own_bytes() {
        let dir = temp_dir();
        let old = extract_into(&dir, "tp_plug.py", "# old\n").unwrap();
        let new = extract_into(&dir, "tp_plug.py", "# new\n").unwrap();
        assert_ne!(old, new);
        // The old version spawning again finds its own copy untouched.
        assert_eq!(extract_into(&dir, "tp_plug.py", "# old\n").unwrap(), old);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "# old\n");
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "# new\n");
        let name = new.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("tp_plug.") && name.ends_with(".py"),
            "{name}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_copy_is_rewritten() {
        let dir = temp_dir();
        let path = extract_into(&dir, "tp_worker.py", "# v1\n").unwrap();
        std::fs::write(&path, "# trunc").unwrap();
        assert_eq!(extract_into(&dir, "tp_worker.py", "# v1\n").unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# v1\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_removes_only_untouched_copies_of_other_versions() {
        let dir = temp_dir();
        let current = extract_into(&dir, "tp_plug.py", "# current\n").unwrap();
        let other = extract_into(&dir, "tp_plug.py", "# other\n").unwrap();
        let worker = extract_into(&dir, "tp_worker.py", "# other\n").unwrap();
        let legacy = dir.join("tp_plug.py");
        std::fs::write(&legacy, "# pre-hash CLI").unwrap();

        prune_stale_copies(&dir, "tp_plug.py", &current, STALE_SCRIPT_AGE);
        assert!(
            other.exists(),
            "a copy another version just used was deleted"
        );

        prune_stale_copies(&dir, "tp_plug.py", &current, std::time::Duration::ZERO);
        assert!(!other.exists(), "a stale copy is still pruned");
        assert!(current.exists());
        assert!(worker.exists(), "another script's copy was pruned");
        assert!(legacy.exists(), "an older CLI's un-hashed copy was pruned");
        std::fs::remove_dir_all(&dir).ok();
    }
}
