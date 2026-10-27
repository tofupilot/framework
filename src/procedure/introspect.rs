//! Phase-signature introspection via the Python worker.
//!
//! Spawns `tp_worker.py <procedure_dir> --introspect` once, which imports
//! every Python phase callable exactly the way execution would and reports
//! its real parameter names (`inspect.signature`). This is what lets a
//! partial run start only the plugs its phase set can actually touch,
//! without a Rust-side signature parser and its whole class of bugs
//! (multi-line signatures, decorators, `*args`, defaults).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::schema::{PhaseDefinition, ProcedureDefinition};

/// Plug keys a phase can touch, as decided by `plug_keys_for_phase`:
/// - `Some(keys)` — provably exact (possibly empty) set of plug keys
/// - `None` — genuinely unknown (module failed to import); the caller
///   must fall back to all plugs so degradation over-starts instead of
///   silently under-starting.
pub type PhasePlugs = Option<Vec<String>>;

/// One phase's introspection result, as reported by the worker.
#[derive(Debug, Deserialize)]
pub struct PhaseSignature {
    /// Parameter names of the phase callable, in declaration order.
    #[serde(default)]
    pub params: Option<Vec<String>>,
    /// Import / lookup error. Per-phase errors are reported, not fatal:
    /// a phase outside the partial set must not stop the run.
    #[serde(default)]
    pub error: Option<String>,
}

/// Signatures for every Python phase of a procedure, keyed by phase key.
#[derive(Debug, Default, Deserialize)]
pub struct Introspection {
    #[serde(default)]
    pub phases: HashMap<String, PhaseSignature>,
}

impl Introspection {
    pub fn get(&self, phase_key: &str) -> Option<&PhaseSignature> {
        self.phases.get(phase_key)
    }
}

/// One method of a plug class, as reported by the worker's `"plugs"`
/// introspection: the class's own methods (`vars(cls)`), source order,
/// `__init__` and underscore methods included.
#[derive(Debug, Clone, Deserialize)]
pub struct PlugMethodInfo {
    pub name: String,
    /// First non-empty line of the method's own docstring.
    #[serde(default)]
    pub doc: Option<String>,
    /// Line of the `def` in the source file (the sort key).
    pub lineno: u32,
    /// The file that defines it (a parent class's for an inherited one).
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub params: Vec<PlugParamInfo>,
}

/// One parameter of a plug method, `self` excluded.
#[derive(Debug, Clone, Deserialize)]
pub struct PlugParamInfo {
    pub name: String,
    /// As the CALL accepts it, not as the `def` declares it (tp_worker.py,
    /// `PLUG_CALL_ACCEPTS_KEYWORDS`):
    /// `positional_or_keyword` | `keyword_only` | `positional_only` |
    /// `var_positional` | `var_keyword`.
    pub kind: String,
    /// As the `def` declares it: what a direct call accepts (`tofupilot
    /// plug call --kwargs`). Absent from a worker older than the field.
    #[serde(default)]
    pub declared_kind: Option<String>,
    /// No default and not var_* — a value must be supplied.
    pub required: bool,
    /// The signature default as JSON, when representable.
    #[serde(default)]
    pub default_json: Option<String>,
    /// The annotation's display string, when annotated.
    #[serde(default)]
    pub annotation: Option<String>,
}

/// One plug's introspection result: methods, or the import/lookup error.
#[derive(Debug, Deserialize)]
struct PlugIntrospection {
    #[serde(default)]
    methods: Option<Vec<PlugMethodInfo>>,
    #[serde(default)]
    error: Option<String>,
}

/// The worker's reply to a plugs-only introspection request.
#[derive(Debug, Deserialize)]
struct PlugIntrospectionReply {
    #[serde(default)]
    plugs: HashMap<String, PlugIntrospection>,
}

/// Upper bound on the whole introspection pass. Importing phase modules
/// runs their module-level code, which can block (e.g. opening a serial
/// port that never answers); a hung import must not hang run startup
/// forever. Callers treat the error like any other introspection failure
/// and fall back to all plugs.
const INTROSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Spawn the worker once in `--introspect` mode and collect the signature
/// of every Python phase in the procedure.
///
/// `python_path` follows the same contract as the orchestrator: a
/// pre-resolved interpreter when the caller has one (CLI runs), otherwise
/// the engine's venv walk-up resolves it.
pub async fn introspect_procedure(
    procedure_dir: &Path,
    python_path: &Option<std::path::PathBuf>,
    procedure: &ProcedureDefinition,
) -> Result<Introspection, String> {
    let mut phases = serde_json::Map::new();
    for (_, phase) in procedure.iter_phases_with_stage() {
        if let Some(spec) = &phase.python {
            phases.insert(
                phase.key.clone(),
                serde_json::json!({
                    "module": spec.get_module(),
                    "function": spec.get_callable_name(),
                }),
            );
        }
    }
    if phases.is_empty() {
        return Ok(Introspection::default());
    }

    let request = serde_json::json!({ "phases": phases }).to_string();
    let stdout = run_introspection_worker(procedure_dir, python_path, &request).await?;

    serde_json::from_slice(&stdout).map_err(|e| {
        format!(
            "Failed to parse introspection output: {} (stdout: {})",
            e,
            String::from_utf8_lossy(&stdout)
        )
    })
}

/// Introspect one plug class's own methods, through the same worker
/// `--introspect` pass phases use. Errors carry the Python error text
/// (import failure, missing class) — that IS what the caller shows for
/// a broken plug file.
pub async fn introspect_plug_methods(
    python_path: &Option<std::path::PathBuf>,
    procedure_dir: &Path,
    module: &str,
    class: &str,
) -> Result<Vec<PlugMethodInfo>, String> {
    let request = serde_json::json!({
        "phases": {},
        "plugs": { "target": { "module": module, "class": class } },
    })
    .to_string();
    let stdout = run_introspection_worker(procedure_dir, python_path, &request).await?;

    let mut reply: PlugIntrospectionReply = serde_json::from_slice(&stdout).map_err(|e| {
        format!(
            "Failed to parse introspection output: {} (stdout: {})",
            e,
            String::from_utf8_lossy(&stdout)
        )
    })?;
    let entry = reply
        .plugs
        .remove("target")
        .ok_or_else(|| "Introspection worker reported nothing for the plug".to_string())?;
    if let Some(error) = entry.error {
        return Err(error);
    }
    entry
        .methods
        .ok_or_else(|| "Introspection worker reported neither methods nor an error".to_string())
}

/// Spawn the worker in `--introspect` mode, feed it `request` on stdin
/// and return its stdout. Shared by phase and plug introspection: same
/// interpreter resolution, timeout and kill discipline.
async fn run_introspection_worker(
    procedure_dir: &Path,
    python_path: &Option<std::path::PathBuf>,
    request: &str,
) -> Result<Vec<u8>, String> {
    let python_cmd = crate::python::resolve_or_walk(python_path, procedure_dir).await?;
    let worker_script = crate::worker::Worker::find_worker_script_cli()?;
    let abs_dir = crate::path_utils::canonicalize_for_spawn(procedure_dir)
        .map_err(|e| format!("Failed to canonicalize procedure dir: {}", e))?;

    let mut cmd = tokio::process::Command::new(&python_cmd);
    // Introspection imports the procedure's modules, so it runs user
    // code: no credentials, like every other worker.
    crate::procedure_env::strip_inherited(&mut cmd);
    let mut child = cmd
        .arg(&worker_script)
        .arg(&abs_dir)
        .arg("--introspect")
        .current_dir(&abs_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            format!(
                "cannot start {}: {}",
                std::path::Path::new(&python_cmd).display(),
                crate::path_utils::io_reason(&e)
            )
        })?;

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or("Introspection worker has no stdin")?;
        stdin
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Failed to write introspection request: {}", e))?;
        // Dropping stdin closes the pipe so the worker's json.load(sys.stdin) returns.
    }

    let output = tokio::time::timeout(INTROSPECT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "Introspection timed out after {}s (module-level code blocking at import?)",
                INTROSPECT_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to run introspection worker: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Introspection worker exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(output.stdout)
}

impl ProcedureDefinition {
    /// Which of this procedure's plug keys `phase` can actually touch.
    ///
    /// | Case | Value | Why |
    /// |---|---|---|
    /// | No callable (native, UI-only, shell phase) | `Some(vec![])` | Provably zero plugs — no function to take a parameter |
    /// | Python phase, signature read | `Some([...])` | Parameters ∩ declared plug keys, compared lowercased |
    /// | Python phase, module failed to import | `None` | Genuinely unknown; caller falls back to all plugs |
    ///
    /// The lowercased comparison mirrors the worker's own plug matching
    /// (`param_name.lower() == plug_key.lower()` in tp_worker.py) — a
    /// case-sensitive intersection would decide the phase needs no plug
    /// and let it fail deep inside instead of at startup.
    ///
    /// The result is deliberately a superset of what the phase will be
    /// handed: the worker resolves `phase_results` before plugs, so a
    /// parameter naming both a completed phase and a plug resolves to
    /// the phase result. Over-starting that plug is harmless.
    pub fn plug_keys_for_phase(
        &self,
        phase: &PhaseDefinition,
        introspection: &Introspection,
    ) -> PhasePlugs {
        if phase.python.is_none() {
            return Some(Vec::new());
        }

        match introspection.get(&phase.key) {
            Some(PhaseSignature {
                params: Some(params),
                ..
            }) => Some(
                self.plugs
                    .iter()
                    .filter(|plug| {
                        params.iter().any(|p| {
                            // A phase of a sequence may name a plug by the
                            // requirement key the call bound to this plug.
                            let wanted = phase.plug_aliases.get(p).map(String::as_str).unwrap_or(p);
                            wanted.eq_ignore_ascii_case(&plug.key)
                        })
                    })
                    .map(|plug| plug.key.clone())
                    .collect(),
            ),
            // Reported error, or a Python phase the worker never answered
            // for: unknown either way.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procedure(yaml: &str) -> ProcedureDefinition {
        let raw: crate::procedure::schema::ProcedureYaml =
            crate::procedure::yaml::from_str(yaml).unwrap();
        ProcedureDefinition::try_from(raw).unwrap()
    }

    fn introspection(entries: &[(&str, PhaseSignature)]) -> Introspection {
        Introspection {
            phases: entries
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        PhaseSignature {
                            params: v.params.clone(),
                            error: v.error.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn params(names: &[&str]) -> PhaseSignature {
        PhaseSignature {
            params: Some(names.iter().map(|s| s.to_string()).collect()),
            error: None,
        }
    }

    const TWO_PLUG_PROCEDURE: &str = r#"
name: test
version: 1.0.0
plugs:
  - name: power_supply
    python: plugs.power_supply:PowerSupply
  - name: multimeter
    python: plugs.multimeter:Multimeter
main:
  - key: with_plug
    name: With plug
    python: phases.a:with_plug
  - key: builtins_only
    name: Builtins only
    python: phases.a:builtins_only
  - key: no_callable
    name: No callable
"#;

    #[test]
    fn plug_hit_intersects_params_with_plug_keys() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        let intr = introspection(&[("with_plug", params(&["measurements", "power_supply"]))]);
        let phase = def.main.iter().find(|p| p.key == "with_plug").unwrap();
        assert_eq!(
            def.plug_keys_for_phase(phase, &intr),
            Some(vec!["power_supply".to_string()])
        );
    }

    #[test]
    fn builtin_params_match_no_plug() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        let intr = introspection(&[(
            "builtins_only",
            params(&[
                "measurements",
                "run",
                "phase",
                "log",
                "ui",
                "unit",
                "attach",
            ]),
        )]);
        let phase = def.main.iter().find(|p| p.key == "builtins_only").unwrap();
        assert_eq!(def.plug_keys_for_phase(phase, &intr), Some(vec![]));
    }

    #[test]
    fn phase_results_param_is_not_a_plug() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        // `other_phase` names a completed phase, not a plug: it must not
        // survive the intersection.
        let intr = introspection(&[("with_plug", params(&["phase", "other_phase"]))]);
        let phase = def.main.iter().find(|p| p.key == "with_plug").unwrap();
        assert_eq!(def.plug_keys_for_phase(phase, &intr), Some(vec![]));
    }

    #[test]
    fn plug_key_matching_is_case_insensitive() {
        // Plug declared with a capitalized key; the worker resolves the
        // lowercase parameter to it, so the intersection must too.
        let def = procedure(
            r#"
name: test
version: 1.0.0
plugs:
  - key: Power_Supply
    name: Power supply
    python: plugs.power_supply:PowerSupply
main:
  - key: with_plug
    name: With plug
    python: phases.a:with_plug
"#,
        );
        let intr = introspection(&[("with_plug", params(&["power_supply"]))]);
        let phase = def.main.iter().find(|p| p.key == "with_plug").unwrap();
        assert_eq!(
            def.plug_keys_for_phase(phase, &intr),
            Some(vec!["Power_Supply".to_string()])
        );
    }

    #[test]
    fn phase_without_callable_needs_no_plugs() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        // Not even present in the introspection map — there is nothing
        // to introspect. Must still be Some(vec![]), not None.
        let intr = introspection(&[]);
        let phase = def.main.iter().find(|p| p.key == "no_callable").unwrap();
        assert_eq!(def.plug_keys_for_phase(phase, &intr), Some(vec![]));
    }

    #[test]
    fn import_error_is_unknown() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        let intr = introspection(&[(
            "with_plug",
            PhaseSignature {
                params: None,
                error: Some("No module named 'serial'".to_string()),
            },
        )]);
        let phase = def.main.iter().find(|p| p.key == "with_plug").unwrap();
        assert_eq!(def.plug_keys_for_phase(phase, &intr), None);
    }

    #[test]
    fn python_phase_missing_from_introspection_is_unknown() {
        let def = procedure(TWO_PLUG_PROCEDURE);
        let intr = introspection(&[]);
        let phase = def.main.iter().find(|p| p.key == "with_plug").unwrap();
        assert_eq!(def.plug_keys_for_phase(phase, &intr), None);
    }

    /// The worker's `"plugs"` reply, exactly as tp_worker.py prints it:
    /// `__init__` and underscore methods, JSON and non-JSON defaults,
    /// annotations present and absent.
    #[test]
    fn plug_methods_reply_parses() {
        let json = r#"{
            "phases": {},
            "plugs": {
                "target": {
                    "methods": [
                        {
                            "name": "__init__",
                            "doc": null,
                            "lineno": 4,
                            "params": [
                                {"name": "port", "kind": "positional_or_keyword", "required": false, "default_json": "\"/dev/ttyUSB0\"", "annotation": "str"},
                                {"name": "session", "kind": "keyword_only", "required": false, "default_json": null, "annotation": null}
                            ]
                        },
                        {
                            "name": "measure",
                            "doc": "Read one voltage sample.",
                            "lineno": 9,
                            "params": [
                                {"name": "channel", "kind": "positional_or_keyword", "required": true, "default_json": null, "annotation": "int"},
                                {"name": "extras", "kind": "var_keyword", "required": false, "default_json": null, "annotation": null}
                            ]
                        },
                        {"name": "_flush", "doc": null, "lineno": 14, "params": []}
                    ]
                }
            }
        }"#;
        let reply: PlugIntrospectionReply = serde_json::from_str(json).unwrap();
        let entry = &reply.plugs["target"];
        assert!(entry.error.is_none());
        let methods = entry.methods.as_ref().unwrap();
        assert_eq!(
            methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["__init__", "measure", "_flush"],
        );
        let init = &methods[0];
        assert_eq!(init.lineno, 4);
        assert_eq!(
            init.params[0].default_json.as_deref(),
            Some("\"/dev/ttyUSB0\"")
        );
        assert!(!init.params[0].required);
        assert_eq!(init.params[1].kind, "keyword_only");
        // A non-JSON default: optional, but the value stays unstated.
        assert!(init.params[1].default_json.is_none());
        let measure = &methods[1];
        assert_eq!(measure.doc.as_deref(), Some("Read one voltage sample."));
        assert!(measure.params[0].required);
        assert_eq!(measure.params[0].annotation.as_deref(), Some("int"));
        assert!(!measure.params[1].required, "var_keyword is never required");
    }

    /// A broken plug file reports its error per entry, like phases do.
    #[test]
    fn plug_methods_error_entry_parses() {
        let json = r#"{"phases": {}, "plugs": {"target": {"error": "No module named 'serial'"}}}"#;
        let reply: PlugIntrospectionReply = serde_json::from_str(json).unwrap();
        let entry = &reply.plugs["target"];
        assert_eq!(entry.error.as_deref(), Some("No module named 'serial'"));
        assert!(entry.methods.is_none());
    }
}
