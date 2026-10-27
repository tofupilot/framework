//! Typed deployment manifest. Schema produced by
//! `apps/web/server/git/clone-context.ts` and consumed by the CLI's pull /
//! install / run paths. Replaces the prior ad-hoc
//! `manifest.get("...").and_then(|v| v.as_str())` reads so unknown
//! shapes fail loudly at parse time and every read site shares one
//! validator.
//!
//! Schema v1 is the only shape this CLI accepts. The pre-v1 wheel-based
//! bundles (with `package` / `module` / `framework` fields) never
//! escaped the openhtf-source-shipping branch, so there's nothing to
//! migrate from. When the schema bumps to v2 we'll add a `Manifest::V2`
//! arm; until then a missing or non-v1 manifest is a hard error.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manifest {
    V1(V1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum V1Kind {
    /// Procedure ships as a source tree under `bundle/project/`. No
    /// procedure wheel; framework is detected at run time. Today this is
    /// the only kind; the variant exists so a future `Wheel` kind doesn't
    /// require another schema version bump.
    Source(SourceManifest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1 {
    /// Schema version. Always `1` for this enum; the parser hard-fails on
    /// any other value so a v2 bundle reaches a CLI that can't read it
    /// with a clear "unsupported manifest version" error.
    pub version: u32,
    #[serde(flatten)]
    pub kind: V1Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Sync,
    Standalone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub mode: Mode,
    /// Subdirectory of the deployment dir where the procedure's
    /// `pyproject.toml` lives. `None` is the legacy single-package layout
    /// (deployment root). Validated against `is_safe_root_directory`.
    #[serde(default)]
    pub root_directory: Option<String>,
    /// Resolved CPython version, X.Y.Z. The station hands this to
    /// `uv venv --python <version>`; uv downloads the matching PBS build
    /// on first use and caches it globally.
    pub runtime_version: String,
    /// Standalone-only — wheel-tag set the wheelhouse was built for.
    /// Sync bundles set this to `null` since the station resolves wheels
    /// for its own arch at install time.
    #[serde(default)]
    pub platform: Option<String>,
    /// Entry-point path inside the procedure's package dir, relative to
    /// it. The CLI joins this onto the package dir and hands the result
    /// to the framework connector. `None` means "use the framework
    /// default" (openhtf/plain → main.py, pytest → ".", yaml →
    /// `procedure.yaml` auto-discovery), which is what older bundles
    /// without this field also resolve to. Validated against
    /// [`is_safe_entry_point`].
    #[serde(default)]
    pub entry_point: Option<String>,
}

#[derive(Debug)]
pub enum ManifestError {
    Read {
        path: String,
        source: std::io::Error,
    },
    Json {
        path: String,
        source: serde_json::Error,
    },
    SchemaV1 {
        path: String,
        source: serde_json::Error,
    },
    UnsupportedVersion {
        path: String,
        version: u64,
    },
    InvalidField {
        path: String,
        field: &'static str,
        reason: String,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Read { path, source } => {
                write!(f, "Cannot read manifest {path}: {source}")
            }
            ManifestError::Json { path, source } => {
                write!(f, "Cannot parse manifest {path}: {source}")
            }
            ManifestError::SchemaV1 { path, source } => {
                write!(f, "Invalid manifest {path}: {source}")
            }
            ManifestError::UnsupportedVersion { path, version } => write!(
                f,
                "Manifest {path}: version {version} unsupported; run `tofupilot update`"
            ),
            ManifestError::InvalidField {
                path,
                field,
                reason,
            } => write!(f, "Invalid manifest {path}: {field}: {reason}"),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ManifestError::Read { source, .. } => Some(source),
            ManifestError::Json { source, .. } | ManifestError::SchemaV1 { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

impl Manifest {
    /// Read + parse a manifest.json. Missing file, missing `version`, or
    /// any version other than 1 is a hard error — the new pipeline
    /// always emits a v1 manifest, so a bundle without one is corrupt
    /// or from a future CLI we don't know how to read.
    pub fn parse(path: &Path) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path).map_err(|e| ManifestError::Read {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::parse_bytes(path, &bytes)
    }

    /// Parse from in-memory bytes. Public so tests can exercise the parser
    /// without touching the filesystem.
    pub fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<Self, ManifestError> {
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|source| ManifestError::Json {
                path: path.display().to_string(),
                source,
            })?;

        // Route by `version`. Only `1` is accepted; anything else is a
        // hard error so a future bundle on an old CLI doesn't get
        // silently misread, and a corrupt / pre-v1 manifest doesn't get
        // accepted as a no-op.
        match raw.get("version").and_then(|v| v.as_u64()) {
            Some(1) => {
                let v1: V1 =
                    serde_json::from_value(raw).map_err(|source| ManifestError::SchemaV1 {
                        path: path.display().to_string(),
                        source,
                    })?;
                v1.validate(path)?;
                Ok(Manifest::V1(v1))
            }
            Some(other) => Err(ManifestError::UnsupportedVersion {
                path: path.display().to_string(),
                version: other,
            }),
            None => Err(ManifestError::InvalidField {
                path: path.display().to_string(),
                field: "version",
                reason: "missing — manifest must declare schema version (e.g. \"version\": 1)"
                    .into(),
            }),
        }
    }

    pub fn root_directory(&self) -> Option<&str> {
        match self {
            Manifest::V1(V1 {
                kind: V1Kind::Source(s),
                ..
            }) => s.root_directory.as_deref(),
        }
    }

    pub fn entry_point(&self) -> Option<&str> {
        match self {
            Manifest::V1(V1 {
                kind: V1Kind::Source(s),
                ..
            }) => s.entry_point.as_deref(),
        }
    }
}

impl V1 {
    /// Field-level validation that serde can't express. Mirrors the
    /// server-side `procedure_root_directory_safe` Drizzle CHECK
    /// constraint and the deployer's regex; the rule itself lives in
    /// [`is_safe_root_directory`] below.
    fn validate(&self, path: &Path) -> Result<(), ManifestError> {
        let path_str = || path.display().to_string();
        match &self.kind {
            V1Kind::Source(s) => {
                if let Some(pd) = &s.root_directory {
                    if !is_safe_root_directory(pd) {
                        return Err(ManifestError::InvalidField {
                            path: path_str(),
                            field: "root_directory",
                            reason: format!("unsafe value: {pd:?}"),
                        });
                    }
                }
                if let Some(ep) = &s.entry_point {
                    if !is_safe_entry_point(ep) {
                        return Err(ManifestError::InvalidField {
                            path: path_str(),
                            field: "entry_point",
                            reason: format!("unsafe value: {ep:?}"),
                        });
                    }
                }
                if !is_valid_runtime_version(&s.runtime_version) {
                    return Err(ManifestError::InvalidField {
                        path: path_str(),
                        field: "runtime_version",
                        reason: format!("expected X.Y.Z, got {:?}", s.runtime_version),
                    });
                }
                // `platform` is freeform string; the install path checks
                // against its own enum (the CLI may know about platforms
                // the build script didn't, e.g. cross-version rollouts).
                // Empty string is invalid because it's user-confusing.
                if let Some(p) = &s.platform {
                    if p.is_empty() || p.len() > 64 {
                        return Err(ManifestError::InvalidField {
                            path: path_str(),
                            field: "platform",
                            reason: "must be non-empty and ≤ 64 chars".into(),
                        });
                    }
                }
                Ok(())
            }
        }
    }
}

/// Repo-relative path safety. Same rules as
/// `apps/web/drizzle/schema/procedure.ts`'s
/// `procedure_root_directory_safe` CHECK constraint. Mirroring the
/// rule on the CLI side defends against a tampered manifest reaching
/// `Path::join` (an absolute value would replace the base).
pub fn is_safe_root_directory(value: &str) -> bool {
    if value.is_empty() || value.len() > 256 {
        return false;
    }
    if value.starts_with('/') || value.contains('\\') {
        return false;
    }
    for segment in value.split('/') {
        if segment.is_empty() {
            return false;
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
        {
            return false;
        }
    }
    if value.split('/').any(|s| s == ".." || s == ".") {
        return false;
    }
    true
}

/// Entry-point path safety. Same shape as `is_safe_root_directory`
/// with one extra exception: a single `.` is valid (= "the package dir
/// itself", which is what pytest's discovery surface expects). Mirror
/// of the `procedure_entry_point_safe` Drizzle CHECK and the JS
/// `validateEntryPoint`. The CLI joins this onto the validated package
/// dir before reaching `Path::join`, so the same safety rules apply.
pub fn is_safe_entry_point(value: &str) -> bool {
    if value == "." {
        return true;
    }
    is_safe_root_directory(value)
}

/// X.Y.Z, each component ≤ 4 digits. Tighter than the build sandbox's
/// `sys.version_info` output (always X.Y.Z) so a tampered manifest with
/// `runtime_version: "rm -rf /"` can't reach the `uv venv --python` shell
/// arg.
fn is_valid_runtime_version(value: &str) -> bool {
    if value.len() > 16 {
        return false;
    }
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("/test/manifest.json")
    }

    #[test]
    fn parses_v1_source_sync_single_package() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":null,"runtime_version":"3.12.13","platform":null}"#;
        let Manifest::V1(V1 {
            version,
            kind: V1Kind::Source(s),
        }) = Manifest::parse_bytes(&p(), bytes).unwrap();
        assert_eq!(version, 1);
        assert!(matches!(s.mode, Mode::Sync));
        assert_eq!(s.root_directory, None);
        assert_eq!(s.runtime_version, "3.12.13");
        assert_eq!(s.platform, None);
    }

    #[test]
    fn parses_v1_source_standalone_monorepo() {
        let bytes = br#"{
            "version": 1,
            "kind": "source",
            "mode": "standalone",
            "root_directory": "procedures/laptop-fvt",
            "runtime_version": "3.12.0",
            "platform": "linux_x86_64"
        }"#;
        let m = Manifest::parse_bytes(&p(), bytes).unwrap();
        assert_eq!(m.root_directory(), Some("procedures/laptop-fvt"));
        let Manifest::V1(V1 {
            kind: V1Kind::Source(s),
            ..
        }) = m;
        assert!(matches!(s.mode, Mode::Standalone));
        assert_eq!(s.platform.as_deref(), Some("linux_x86_64"));
    }

    #[test]
    fn round_trip_serializes_to_same_shape() {
        let original = V1 {
            version: 1,
            kind: V1Kind::Source(SourceManifest {
                mode: Mode::Sync,
                root_directory: Some("apps/foo".into()),
                runtime_version: "3.12.13".into(),
                platform: None,
                entry_point: Some("main.py".into()),
            }),
        };
        let json = serde_json::to_string(&original).unwrap();
        let Manifest::V1(parsed) = Manifest::parse_bytes(&p(), json.as_bytes()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn missing_version_is_hard_error() {
        let bytes = br#"{"mode":"sync","package":"my-proc","module":"my_proc","python":"3.12"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::InvalidField {
                    field: "version",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn unknown_version_is_hard_error() {
        let bytes = br#"{"version":42,"kind":"source","mode":"sync","runtime_version":"3.12.0"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::UnsupportedVersion { version: 42, .. }
        ));
    }

    #[test]
    fn unknown_kind_is_hard_error() {
        let bytes = br#"{"version":1,"kind":"wheel","mode":"sync","runtime_version":"3.12.0"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(
            matches!(err, ManifestError::SchemaV1 { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn ignores_unknown_top_level_fields() {
        // The deployer reads `language` from manifest.json sidecar;
        // the CLI parser must tolerate it (and any future field the
        // deployer / web side adds without bumping the schema version).
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","language":"python","root_directory":null,"runtime_version":"3.12.13","platform":null,"future_field":42}"#;
        let m = Manifest::parse_bytes(&p(), bytes).unwrap();
        let Manifest::V1(V1 {
            kind: V1Kind::Source(s),
            ..
        }) = m;
        assert_eq!(s.runtime_version, "3.12.13");
    }

    #[test]
    fn unknown_mode_is_hard_error() {
        let bytes = br#"{"version":1,"kind":"source","mode":"hybrid","runtime_version":"3.12.0"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(
            matches!(err, ManifestError::SchemaV1 { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_traversal_in_root_directory() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":"../etc","runtime_version":"3.12.0"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "root_directory",
                ..
            }
        ));
    }

    #[test]
    fn rejects_absolute_root_directory() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":"/etc","runtime_version":"3.12.0"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "root_directory",
                ..
            }
        ));
    }

    #[test]
    fn rejects_bogus_runtime_version() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","runtime_version":"3.12"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "runtime_version",
                ..
            }
        ));
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","runtime_version":"a.b.c"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "runtime_version",
                ..
            }
        ));
    }

    #[test]
    fn missing_file_is_error() {
        let err = Manifest::parse(Path::new("/definitely/does/not/exist.json")).unwrap_err();
        assert!(matches!(err, ManifestError::Read { .. }), "got: {err:?}");
    }

    #[test]
    fn parses_entry_point_when_present() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":null,"runtime_version":"3.12.13","platform":null,"entry_point":"main.py"}"#;
        let m = Manifest::parse_bytes(&p(), bytes).unwrap();
        assert_eq!(m.entry_point(), Some("main.py"));
    }

    #[test]
    fn entry_point_defaults_to_none_when_absent() {
        // Older bundles emitted before the entry_point field existed
        // must still parse cleanly. Field absence ⇄ "use the framework
        // default at run time".
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":null,"runtime_version":"3.12.13","platform":null}"#;
        let m = Manifest::parse_bytes(&p(), bytes).unwrap();
        assert_eq!(m.entry_point(), None);
    }

    #[test]
    fn entry_point_dot_is_valid() {
        // pytest's discovery surface — "scan the package dir itself".
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","root_directory":null,"runtime_version":"3.12.13","platform":null,"entry_point":"."}"#;
        let m = Manifest::parse_bytes(&p(), bytes).unwrap();
        assert_eq!(m.entry_point(), Some("."));
    }

    #[test]
    fn rejects_traversal_in_entry_point() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","runtime_version":"3.12.0","entry_point":"../etc"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "entry_point",
                ..
            }
        ));
    }

    #[test]
    fn rejects_absolute_entry_point() {
        let bytes = br#"{"version":1,"kind":"source","mode":"sync","runtime_version":"3.12.0","entry_point":"/etc/passwd"}"#;
        let err = Manifest::parse_bytes(&p(), bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::InvalidField {
                field: "entry_point",
                ..
            }
        ));
    }

    #[test]
    fn safe_entry_point_table() {
        for (input, expected) in [
            (".", true),
            ("main.py", true),
            ("tests/foo.py", true),
            ("..", false),
            ("../etc", false),
            ("./foo", false),
            ("/abs", false),
            ("foo bar.py", false),
        ] {
            assert_eq!(
                is_safe_entry_point(input),
                expected,
                "is_safe_entry_point({input:?}) expected {expected}",
            );
        }
    }

    #[test]
    fn safe_root_directory_table() {
        for (input, expected) in [
            ("foo", true),
            ("apps/foo", true),
            ("a/b/c", true),
            ("foo.bar", true),
            ("foo-bar_baz", true),
            ("", false),
            ("/abs", false),
            ("../etc", false),
            ("./foo", false),
            ("foo/../bar", false),
            ("foo//bar", false),
            ("foo bar", false),
            ("foo\\bar", false),
            ("foo\nbar", false),
        ] {
            assert_eq!(
                is_safe_root_directory(input),
                expected,
                "is_safe_root_directory({input:?}) expected {expected}",
            );
        }
    }
}
