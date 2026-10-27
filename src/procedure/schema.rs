use super::codes;
use crate::events::PlugScope;
use crate::measurements::issue::{DeclaredPart, MatchIssue};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use validator::Validate;

pub use super::sequence::{
    InputSpecYaml, InputType, PhaseSource, PlugRequirementYaml, SequenceInstance, SequenceYaml,
};
pub use super::values::{ConfigSpecYaml, SecretSpecYaml, ValueType};

/// The procedure YAML as a JSON Schema: every field the loader accepts,
/// with the doc comment on the Rust field as its `description`. This is
/// the ONE inventory of the format. The Studio assistant reads the field
/// list from it (through the daemon, so the list matches the engine that
/// will run the file), `export-schema` commits a copy for the web and the
/// CI drift check, and a test refuses a field without a description: a
/// field added here reaches the assistant on its own, or fails the build.
#[cfg(feature = "json-schema")]
pub fn procedure_json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(ProcedureYaml);
    serde_json::to_value(schema).expect("a JSON Schema serializes")
}

/// The aspect ratios an `image` (or an image grid) accepts, as
/// `validate_aspect` checks them and as the schema lists them: one
/// list, so the assistant is never told a value the loader refuses.
pub const IMAGE_ASPECTS: &[&str] = &["16/9", "4/3", "3/4", "2/3", "9/16", "square", "auto"];

/// The object-fit values an `image` accepts (`validate_fit`), same rule.
pub const IMAGE_FITS: &[&str] = &["contain", "cover", "fill"];

/// Schema of a closed set of strings kept in a const, so a field typed
/// `Option<String>` at run time still reads as an enum in the schema.
#[cfg(feature = "json-schema")]
fn closed_set_schema(values: &[&str], description: &str) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "enum": values,
        "description": description
    })
}

/// Schema of `aspect`: the values of `IMAGE_ASPECTS`.
#[cfg(feature = "json-schema")]
struct ImageAspectText;

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for ImageAspectText {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ImageAspect".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        closed_set_schema(
            IMAGE_ASPECTS,
            "Aspect ratio of an image or of an image grid's cells: one of these values, nothing else loads.",
        )
    }
}

/// Schema of `fit`: the values of `IMAGE_FITS`.
#[cfg(feature = "json-schema")]
struct ImageFitText;

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for ImageFitText {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ImageFit".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        closed_set_schema(
            IMAGE_FITS,
            "How an image fills its box: one of these values, nothing else loads.",
        )
    }
}

/// Schema of `width`: a percentage, as `validate_width` accepts it.
#[cfg(feature = "json-schema")]
struct PercentText;

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for PercentText {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Percent".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9]+(\\.[0-9]+)?%$",
            "description": "A percentage of the row, above 0 and up to 100, written with the sign: 50%. Not a pixel size."
        })
    }
}

/// Schema of a duration field, which the YAML writes as text
/// (`deserialize_duration` reads it into milliseconds).
#[cfg(feature = "json-schema")]
struct DurationText;

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for DurationText {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Duration".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9]+(ms|s|m|h)( ?[0-9]+(ms|s|m|h))*$",
            "description": "A duration with its unit: 500ms, 30s, 5m, 1h30m. A bare integer is read as MILLISECONDS (30 means 30ms): always write the unit."
        })
    }
}

/// Schema of `execution.slots`, which `deserialize_slots` reads as a
/// count or as a list.
#[cfg(feature = "json-schema")]
#[derive(schemars::JsonSchema)]
#[schemars(untagged)]
#[allow(dead_code)]
enum SlotsSchema {
    /// A slot count, 1-1024, expanded to slot_1..slot_N, zero-padded to the
    /// digit count of N (12 slots: slot_01..slot_12).
    Count(#[schemars(range(min = 1, max = 1024))] u64),
    /// One entry per slot.
    List(Vec<SlotConfigRaw>),
}

pub const DEFAULT_WORKERS: usize = 8;

fn validate_python_identifier(key: &str) -> Result<(), validator::ValidationError> {
    if !crate::python::is_valid_python_identifier(key) {
        return Err(validator::ValidationError::new("invalid_python_identifier"));
    }
    Ok(())
}

/// The tree a deployment of `procedure_dir` ships: the root of the uv
/// workspace enclosing it when one is declared (`[tool.uv.workspace]` in
/// a `pyproject.toml` found walking up, as the build script does), else
/// the procedure directory itself.
pub fn deployable_root(procedure_dir: &Path) -> PathBuf {
    let mut dir = procedure_dir.to_path_buf();
    for _ in 0..16 {
        let pyproject = dir.join("pyproject.toml");
        if let Ok(text) = std::fs::read_to_string(&pyproject) {
            let declares_workspace = text.contains("[tool.uv.workspace]")
                || (text.contains("[tool.uv]") && text.contains("workspace"));
            if declares_workspace {
                return dir;
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    procedure_dir.to_path_buf()
}

/// A phase key: a Python identifier, or several joined by `/` when the
/// phase came through sequence calls (`calibration/dmm/measure`).
pub fn is_valid_phase_key_path(key: &str) -> bool {
    !key.is_empty()
        && key
            .split('/')
            .all(crate::python::is_valid_python_identifier)
}

fn validate_phase_key_path(key: &str) -> Result<(), validator::ValidationError> {
    if !is_valid_phase_key_path(key) {
        return Err(validator::ValidationError::new("invalid_phase_key"));
    }
    Ok(())
}

fn validate_sub_units_config(config: &SubUnitsConfig) -> Result<(), validator::ValidationError> {
    // Validate items is not empty
    if config.0.is_empty() {
        let mut err = validator::ValidationError::new("empty_sub_units_items");
        err.message = Some("empty; add at least 1 item".into());
        return Err(err);
    }

    // Validate no duplicate keys (case-insensitive)
    let mut seen_keys: HashSet<String> = HashSet::new();
    for item in &config.0 {
        let key = item.get_key().to_lowercase();
        if seen_keys.contains(&key) {
            let mut err = validator::ValidationError::new("duplicate_sub_unit_key");
            err.message = Some(format!("duplicate key {} (label {})", key, item.label).into());
            return Err(err);
        }
        seen_keys.insert(key);
    }

    // Validate keys produce valid Python identifiers
    for item in &config.0 {
        let key = item.get_key();
        if key.is_empty() || !crate::python::is_valid_python_identifier(&key) {
            let mut err = validator::ValidationError::new("invalid_sub_unit_key");
            err.message = Some(
                format!(
                    "invalid key `{}` (from label {}): expected a Python identifier",
                    key, item.label
                )
                .into(),
            );
            return Err(err);
        }
    }

    Ok(())
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_stop() -> FirstFailureAction {
    FirstFailureAction::Stop
}

fn default_strategy() -> ExecutionStrategy {
    ExecutionStrategy::PhaseFirst
}

fn default_workers() -> usize {
    DEFAULT_WORKERS
}

fn default_true() -> bool {
    true
}

/// The project's own Python modules, dotted (`plugs.psu`), sorted: what a
/// misspelt `python:` module is compared with. Skips environments and
/// caches, never follows a symlink (a cycle would never end), and gives
/// up after a few thousand entries: a hint is not worth a slow check.
fn project_modules(procedure_dir: &Path) -> Vec<String> {
    const SKIP: &[&str] = &["venv", "__pycache__", "node_modules", "site-packages"];
    const MAX_ENTRIES: usize = 5000;
    let mut out = Vec::new();
    let mut visited = 0;
    let mut stack = vec![procedure_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                out.sort();
                return out;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if kind.is_dir() {
                if !SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(entry.path());
                }
            } else if let Some(stem) = name.strip_suffix(".py").filter(|_| kind.is_file()) {
                if let Ok(rel) = dir.strip_prefix(procedure_dir) {
                    let mut parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect();
                    parts.push(stem.to_string());
                    out.push(parts.join("."));
                }
            }
        }
    }
    out.sort();
    out
}

/// The folders an import from the procedure's venv searches: each
/// `site-packages` of `venv` or `.venv` (next to the procedure or up to
/// three folders above it, as a uv workspace keeps it, or
/// `UV_PROJECT_ENVIRONMENT`), then the folders their `.pth` files list
/// (an editable install: `pip install -e`, a uv workspace member).
fn venv_import_dirs(procedure_dir: &Path) -> Vec<PathBuf> {
    let mut venvs: Vec<PathBuf> = std::env::var_os("UV_PROJECT_ENVIRONMENT")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    for dir in procedure_dir.ancestors().take(4) {
        venvs.push(dir.join("venv"));
        venvs.push(dir.join(".venv"));
    }
    let mut sites = Vec::new();
    for venv in venvs {
        // Windows `Lib\site-packages`, POSIX `lib/pythonX.Y/site-packages`.
        let windows = venv.join("Lib").join("site-packages");
        if windows.is_dir() {
            sites.push(windows);
        }
        if let Ok(entries) = std::fs::read_dir(venv.join("lib")) {
            sites.extend(
                entries
                    .flatten()
                    .map(|e| e.path().join("site-packages"))
                    .filter(|p| p.is_dir()),
            );
        }
    }
    let mut dirs = sites.clone();
    for site in &sites {
        let Ok(entries) = std::fs::read_dir(site) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().ends_with(".pth") {
                if let Ok(text) = std::fs::read_to_string(e.path()) {
                    dirs.extend(
                        text.lines()
                            .map(str::trim)
                            .filter(|l| {
                                !l.is_empty() && !l.starts_with('#') && !l.starts_with("import ")
                            })
                            // A relative entry is relative to the
                            // site-packages folder holding the `.pth`.
                            .map(|l| site.join(l))
                            .filter(|p| p.is_dir()),
                    );
                }
            }
        }
    }
    dirs
}

/// The procedure's `pyproject.toml` or `requirements.txt` (next to it or
/// up to three folders above, as a uv workspace keeps them) names the
/// distribution of `module`'s top package (`acme_drivers` or
/// `acme-drivers`).
fn declares_dependency(procedure_dir: &Path, module: &str) -> bool {
    let top = module.split('.').next().unwrap_or(module).to_lowercase();
    if top.is_empty() {
        return false;
    }
    let dashed = top.replace('_', "-");
    procedure_dir.ancestors().take(4).any(|dir| {
        ["pyproject.toml", "requirements.txt"].iter().any(|f| {
            std::fs::read_to_string(dir.join(f)).is_ok_and(|t| {
                let t = t.to_lowercase();
                // As a whole name: `acme-drivers`, `acme_drivers>=1`, not
                // `acme-drivers-extra`.
                [top.as_str(), dashed.as_str()].iter().any(|name| {
                    t.match_indices(name).any(|(i, _)| {
                        let before = t[..i].chars().next_back();
                        let after = t[i + name.len()..].chars().next();
                        !before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
                            && !after.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
                    })
                })
            })
        })
    })
}

/// The top package of `module` is in the procedure's venv: an import
/// outside the tree that will resolve.
fn installed_in_venv(procedure_dir: &Path, module: &str) -> bool {
    let top = module.split('.').next().unwrap_or(module);
    if top.is_empty() {
        return false;
    }
    venv_import_dirs(procedure_dir).iter().any(|dir| {
        dir.join(top).is_dir()
            || dir.join(format!("{top}.py")).is_file()
            || std::fs::read_dir(dir).is_ok_and(|entries| {
                entries.flatten().any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    // An extension module (`foo.cpython-311-x86_64-linux-gnu.so`, `.pyd`),
                    // or a setuptools editable finder named after it.
                    (name.starts_with(&format!("{top}."))
                        && (name.ends_with(".so") || name.ends_with(".pyd")))
                        // A setuptools editable finder, named after the
                        // distribution (`tofupilot-drivers` for `drivers`):
                        // its `MAPPING` names the packages it serves.
                        || (name.starts_with("__editable__")
                            && name.ends_with("_finder.py")
                            && std::fs::read_to_string(e.path()).is_ok_and(|t| {
                                t.contains(&format!("'{top}':")) || t.contains(&format!("\"{top}\":"))
                            }))
                })
            })
    })
}

/// The source file of `module` in the procedure's venv, when it is plain
/// Python there (`acme_drivers/psu.py`, a package's `__init__.py`): what
/// the lint reads a class from, as it reads a file of the procedure.
fn venv_module_file(procedure_dir: &Path, module: &str) -> Option<PathBuf> {
    let rel: PathBuf = module.split('.').collect();
    venv_import_dirs(procedure_dir).into_iter().find_map(|dir| {
        let file = dir.join(&rel).with_extension("py");
        if file.is_file() {
            return Some(file);
        }
        let init = dir.join(&rel).join("__init__.py");
        init.is_file().then_some(init)
    })
}

/// A sequence's references resolve from the sequence file's folder
/// (`sequence_dir`): when the file the author meant sits in a folder above
/// it (`phases/cal.py` at the procedure root, from `sequences/`), the
/// spelling that reaches it.
fn sequence_parent_hint(
    file: &Path,
    sequence_dir: &Path,
    procedure_dir: &Path,
    callable: &str,
) -> String {
    let Ok(written) = file.strip_prefix(sequence_dir) else {
        return String::new();
    };
    let mut dir = sequence_dir;
    for up in 1.. {
        let Some(parent) = dir.parent() else { break };
        if !parent.starts_with(procedure_dir) {
            break;
        }
        dir = parent;
        if dir.join(written).is_file() {
            let spelled = format!(
                "{}{}",
                "../".repeat(up),
                written.to_string_lossy().replace('\\', "/")
            );
            return format!(
                "; a sequence's `python:` resolves from its own folder: `{spelled}:{callable}` reaches {}",
                relative_to(&dir.join(written), procedure_dir)
            );
        }
    }
    String::new()
}

/// `(module, name)` for each top-level `from` import of `source` (file
/// `file`) naming what a module of the procedure does not define. A
/// module outside the procedure (stdlib, installed), one with a
/// module-level `__getattr__`, `globals()` writes or a `*` import, and a
/// submodule of a package are never reported.
fn unresolved_imports(file: &Path, source: &str, procedure_dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (dots, module, names) in super::pysource::from_imports(source) {
        let mut dir = if dots == 0 {
            // Absolute: only a module of the procedure tree is judged.
            let first = module.split('.').next().unwrap_or_default();
            if first.is_empty()
                || !(procedure_dir.join(first).is_dir()
                    || procedure_dir.join(format!("{first}.py")).is_file())
            {
                continue;
            }
            procedure_dir.to_path_buf()
        } else {
            file.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        for _ in 1..dots {
            dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
        }
        let base = dir.join(module.replace('.', "/"));
        // A regular package wins over a module of the same name, as in
        // Python; a namespace package or an extension module is not read.
        let (target, package_dir) = if module.is_empty() {
            (dir.join("__init__.py"), dir.clone())
        } else if base.join("__init__.py").is_file() {
            (base.join("__init__.py"), base.clone())
        } else {
            (base.with_extension("py"), PathBuf::new())
        };
        let Ok(text) = std::fs::read_to_string(&target) else {
            // No such module at all, where its package is (`lib/` has no
            // `nothere`): a ModuleNotFoundError. A folder (a namespace
            // package) or a compiled module is something Python imports.
            // Only under a regular package that does not extend its own
            // `__path__`: a namespace folder or a package that grows at
            // import time can find the module elsewhere.
            let leaf = module.rsplit('.').next().unwrap_or_default();
            let parent = base.parent().map(Path::to_path_buf).unwrap_or_default();
            let regular = std::fs::read_to_string(parent.join("__init__.py")).is_ok_and(|init| {
                ![
                    "__path__",
                    "extend_path",
                    "sys.modules[",
                    "pkgutil",
                    "importlib.import_module",
                    "importlib.util",
                ]
                .iter()
                .any(|p| init.contains(p))
            });
            if !module.is_empty() && regular && !base.is_dir() && !has_module_file(&parent, leaf) {
                out.push((format!("{}{module}", ".".repeat(dots)), String::new()));
            }
            continue;
        };
        let dynamic = text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("def __getattr__(")
                || l.starts_with("__getattr__ =")
                || l.starts_with("__getattr__=")
                || [
                    "globals()",
                    "vars()",
                    "sys.modules[",
                    "setattr(",
                    "import *",
                ]
                .iter()
                .any(|p| l.contains(p))
        });
        if dynamic {
            continue;
        }
        for name in names {
            let submodule = !package_dir.as_os_str().is_empty()
                && (package_dir.join(&name).is_dir() || has_module_file(&package_dir, &name));
            // Only a name the module never spells (a typo, a name moved
            // away): one defined in a block (`try:`, `if TYPE_CHECKING:`)
            // or by a loop is the module's to know.
            let spelled = text.match_indices(name.as_str()).any(|(i, _)| {
                let before = text[..i].chars().next_back();
                let after = text[i + name.len()..].chars().next();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if !submodule && !spelled && !super::pysource::binds_top_level(&text, &name) {
                let shown = format!("{}{module}", ".".repeat(dots));
                out.push((shown, name));
            }
        }
    }
    out
}

/// `dir/name.py`, or a compiled module (`name.cpython-311-x86_64-linux-gnu.so`,
/// `name.pyd`, a sourceless `name.pyc`).
fn has_module_file(dir: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let file = e.file_name();
        let file = file.to_string_lossy();
        file.split_once('.').is_some_and(|(stem, _)| stem == name)
            && [".py", ".pyc", ".so", ".pyd"]
                .iter()
                .any(|ext| file.ends_with(ext))
    })
}

/// Where `name`, bound in `file` by a `from` import of a module of the
/// procedure (`from .impl import check`, a package `__init__` that
/// re-exports, `from drivers.base import Base`), is defined: the file,
/// its source, and the name there. Followed a few hops; the input when it
/// is defined here or comes from outside the procedure.
fn follow_import(
    file: &Path,
    source: String,
    name: &str,
    procedure_dir: &Path,
) -> (PathBuf, String, String) {
    let (mut file, mut source, mut name) = (file.to_path_buf(), source, name.to_string());
    for _ in 0..4 {
        if super::pysource::top_level_functions(&source).contains(&name)
            || super::pysource::top_level_classes(&source).contains(&name)
        {
            break;
        }
        let Some((dots, module, original)) = super::pysource::relative_import_of(&source, &name)
        else {
            break;
        };
        let mut dir = if dots == 0 {
            procedure_dir.to_path_buf()
        } else {
            file.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        for _ in 1..dots {
            dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
        }
        let base = dir.join(module.replace('.', "/"));
        let target = if module.is_empty() {
            dir.join("__init__.py")
        } else if base.with_extension("py").is_file() {
            base.with_extension("py")
        } else {
            base.join("__init__.py")
        };
        let Ok(text) = std::fs::read_to_string(&target) else {
            break;
        };
        (file, source, name) = (target, text, original);
    }
    (file, source, name)
}

/// Names the phase-side plug proxy (`Plug` in tp_worker.py) keeps for
/// itself: a driver method of that name is out of a phase's reach.
pub const PLUG_PROXY_OWN: &[&str] = &["call", "deadline"];

/// Every name a plug's class has (its own and those of its bases in the
/// procedure), read from its source. `None` when it cannot be known: an
/// installed class, a base from outside the procedure, a `__getattr__`.
fn plug_class_members(plug: &PlugDefinition, procedure_dir: &Path) -> Option<HashSet<String>> {
    let (file, class) = plug.python.parse(procedure_dir).ok()?;
    let source = std::fs::read_to_string(&file).ok()?;
    let mut members = HashSet::new();
    let mut pending = vec![(file, source, class)];
    let mut seen = 0;
    while let Some((file, source, class)) = pending.pop() {
        seen += 1;
        if seen > 8 {
            return None;
        }
        let (file, source, class) = follow_import(&file, source, &class, procedure_dir);
        // Methods attached from outside the body (`setattr(Psu, ...)`).
        if source.contains(&format!("setattr({class}"))
            || source.contains(&format!("setattr( {class}"))
        {
            return None;
        }
        let own = super::pysource::class_members(&source, &class)?;
        if own
            .iter()
            .any(|m| m == "__getattr__" || m == "__getattribute__")
        {
            return None;
        }
        members.extend(own);
        for base in super::pysource::class_bases(&source, &class)? {
            if base == "object" {
                continue;
            }
            if base.contains('.') || base.contains('[') {
                return None;
            }
            pending.push((file.clone(), source.clone(), base));
        }
    }
    Some(members)
}

/// The `__init__` signature `class` gets, read where it is defined: a
/// re-export followed, and an `__init__` inherited from a single base of
/// the procedure (`class Fixture(Instrument)`) read from that base.
fn class_init_signature(
    file: &Path,
    source: String,
    class: &str,
    procedure_dir: &Path,
) -> Option<super::pysource::InitSignature> {
    let (mut file, mut source, mut class) = follow_import(file, source, class, procedure_dir);
    let mut first = None;
    for _ in 0..4 {
        let sig = super::pysource::init_signature(&source, &class);
        if sig.as_ref().is_some_and(|s| s.certain) {
            return sig;
        }
        first = first.or(sig);
        let bases = super::pysource::class_bases(&source, &class)?;
        let [base] = bases.as_slice() else {
            break;
        };
        if base.contains('.') {
            break;
        }
        (file, source, class) = follow_import(&file, source, base, procedure_dir);
        if !super::pysource::top_level_classes(&source).contains(&class) {
            break;
        }
    }
    first
}

fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let usage = || format!("invalid duration `{s}`: expected 500ms, 30s, 5m or 1h30m");

    let mut total_ms = 0f64;
    let mut current_num = String::new();
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else if ch.is_ascii_alphabetic() {
            // The whole word, so `30 sec` names `sec`, not its `e`.
            let mut unit = ch.to_string();
            while let Some(c) = chars.next_if(|c| c.is_ascii_alphabetic()) {
                unit.push(c);
            }
            let factor = match unit.as_str() {
                "ms" => 1.0,
                "s" => 1000.0,
                "m" => 60_000.0,
                "h" => 3_600_000.0,
                other => {
                    let short = match other.to_ascii_lowercase().as_str() {
                        "msec" | "msecs" | "millis" | "millisecond" | "milliseconds" => Some("ms"),
                        "sec" | "secs" | "second" | "seconds" => Some("s"),
                        "min" | "mins" | "minute" | "minutes" => Some("m"),
                        "hr" | "hrs" | "hour" | "hours" => Some("h"),
                        _ => None,
                    };
                    return Err(match short {
                        Some(short) if !current_num.is_empty() => format!(
                            "invalid duration `{s}`: unknown unit `{other}`; write `{current_num}{short}`"
                        ),
                        _ => format!(
                            "invalid duration `{s}`: unknown unit `{other}`; expected ms, s, m or h"
                        ),
                    });
                }
            };
            if current_num.is_empty() {
                return Err(usage());
            }
            let num: f64 = current_num.parse().map_err(|_| usage())?;
            total_ms += num * factor;
            current_num.clear();
        } else if ch == '.' && !current_num.is_empty() {
            // `0.5s`: said in the unit below, which the parser reads.
            let mut fraction = String::new();
            while let Some(c) = chars.next_if(|c| c.is_ascii_digit()) {
                fraction.push(c);
            }
            let unit: String =
                std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_alphabetic())).collect();
            let factor = match unit.as_str() {
                "s" => Some(1000.0),
                "m" => Some(60_000.0),
                "h" => Some(3_600_000.0),
                _ => None,
            };
            let value: Option<f64> = format!("{current_num}.{fraction}").parse().ok();
            return Err(match (value, factor) {
                (Some(v), Some(f)) if (v * f).fract() == 0.0 && v * f > 0.0 => format!(
                    "invalid duration `{s}`: no decimals; write `{}`",
                    format_duration((v * f) as u64)
                ),
                _ if unit == "ms" => format!(
                    "invalid duration `{s}`: no decimals; milliseconds are the smallest unit"
                ),
                _ => format!(
                    "invalid duration `{s}`: no decimals; use a smaller unit (`500ms` for `0.5s`)"
                ),
            });
        } else if ch == '-' {
            return Err(format!(
                "invalid duration `{s}`: a duration cannot be negative"
            ));
        } else if !ch.is_whitespace() {
            return Err(format!(
                "invalid duration `{s}`: unknown unit `{ch}`; expected ms, s, m or h"
            ));
        }
    }

    if !current_num.is_empty() {
        return Err(format!(
            "invalid duration `{s}`: missing unit; expected 500ms, 30s, 5m or 1h30m"
        ));
    }

    if total_ms == 0.0 {
        return Err(format!("invalid duration `{s}`: expected > 0"));
    }

    Ok(total_ms.round() as u64)
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms);
    }

    let total_seconds = ms / 1000;
    let remaining_ms = ms % 1000;

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if secs > 0 {
        parts.push(format!("{}s", secs));
    }
    if remaining_ms > 0 {
        parts.push(format!("{}ms", remaining_ms));
    }

    parts.join("")
}

/// A text field, trimmed. YAML's "nothing" (an empty value, `~`, `null`)
/// is empty text: serde_yaml handed it over as text and serde-saphyr
/// refuses it, but an empty `description:` must keep loading. A required
/// field then fails its own length rule, like an empty `name:` always did.
pub(crate) fn trimmed_text<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|text| text.trim().to_string())
        .unwrap_or_default())
}

/// An `env:` map: every value is text in the process environment, so a
/// number or a boolean written bare (`PORT: 8080`) is read as its text.
pub(crate) fn deserialize_env<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let raw = Option::<BTreeMap<String, serde_json::Value>>::deserialize(deserializer)?;
    raw.unwrap_or_default()
        .into_iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(s) => Ok((key, s)),
            serde_json::Value::Number(n) => Ok((key, n.to_string())),
            serde_json::Value::Bool(b) => Ok((key, b.to_string())),
            _ => Err(D::Error::custom(format!(
                "env.{key}: expected text, a number or a boolean"
            ))),
        })
        .collect()
}

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DurationValue {
        String(String),
        Number(u64),
        Float(f64),
        Bool(bool),
        Other(serde::de::IgnoredAny),
    }

    let input: Option<DurationValue> = Option::deserialize(deserializer)?;
    match input {
        None => Ok(None),
        // `0ms` says what `0` does; a field that needs more than zero
        // refuses both alike.
        Some(DurationValue::String(s)) => match parse_duration(&s) {
            Err(e) if e.ends_with("expected > 0") => Ok(Some(0)),
            parsed => parsed.map(Some).map_err(Error::custom),
        },
        Some(DurationValue::Number(ms)) => Ok(Some(ms)),
        Some(DurationValue::Float(n)) if n < 0.0 => Err(Error::custom(format!(
            "invalid duration `{n}`: a duration cannot be negative"
        ))),
        Some(DurationValue::Float(n)) => Err(Error::custom(format!(
            "invalid duration `{n}`: a bare number counts whole milliseconds; write a unit and no decimals (`1500ms`, `2s`)"
        ))),
        Some(DurationValue::Bool(b)) => Err(Error::custom(format!(
            "invalid duration `{b}`: expected 500ms, 30s, 5m, 1h30m or a whole number of milliseconds"
        ))),
        Some(DurationValue::Other(_)) => Err(Error::custom(
            "invalid duration: expected 500ms, 30s, 5m, 1h30m or a whole number of milliseconds",
        )),
    }
}

fn serialize_duration<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) => serializer.serialize_u64(*ms),
    }
}

fn serialize_duration_as_string<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) => serializer.serialize_str(&format_duration(*ms)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStage {
    Setup,
    Main,
    Teardown,
}

/// Lifetime scope, smallest to largest: `slot < execution < station`.
///
/// The names are 1:1 with existing product vocabulary: a slot maps to
/// one dashboard run (one unit under test), an execution is the whole
/// multi-slot engine execution (`execution_id` on every wire event),
/// and a station is the station process. "run" is deliberately NOT a
/// scope name — a multi-slot execution produces several dashboard
/// runs, so a "run-scoped" plug would read as per-unit while actually
/// spanning all of them.
///
/// On plugs: how long the instrument instance lives and who shares it.
/// On setup/teardown phases: whether the phase runs per slot or once
/// for all slots (station is invalid there — rejected at load).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// One instance per slot, destroyed with the slot.
    Slot,
    /// One instance shared by every slot of one execution, destroyed
    /// with it.
    Execution,
    /// One instance shared by every execution for the life of the
    /// station process. Only valid on plugs, and only held across
    /// executions when a station plug host is present (station mode);
    /// otherwise degrades to `execution` semantics.
    Station,
}

impl Scope {
    /// The word the YAML writes.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Slot => "slot",
            Scope::Execution => "execution",
            Scope::Station => "station",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FirstFailureAction {
    Stop,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    PhaseFirst,
    SlotFirst,
}

// Internal enum for execution engine
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum StageScope {
    SetupAll,
    SetupEach,
    Main,
    TeardownEach,
    TeardownAll,
}

impl StageScope {
    /// Execution order of the stages, as the orchestrator enforces it with
    /// implicit dependencies: a lower rank always completes before a
    /// higher one starts.
    pub fn rank(self) -> u8 {
        match self {
            StageScope::SetupAll => 0,
            StageScope::SetupEach => 1,
            StageScope::Main => 2,
            StageScope::TeardownEach => 3,
            StageScope::TeardownAll => 4,
        }
    }

    /// One job for the whole execution (no slot), as opposed to one per slot.
    pub fn is_shared(self) -> bool {
        matches!(self, StageScope::SetupAll | StageScope::TeardownAll)
    }
}

/// Validate metadata keys against the server-side contract:
/// `^[a-zA-Z0-9_.:+-]+$`, 1..=40 chars, max 50 keys per map.
pub fn validate_metadata_keys<'a>(
    keys: impl Iterator<Item = &'a str>,
    ctx: &str,
) -> Result<(), String> {
    let mut count = 0usize;
    for key in keys {
        count += 1;
        if key.is_empty() || key.len() > 40 {
            return Err(format!(
                "{ctx}: invalid key `{key}`: expected 1-40 characters"
            ));
        }
        if !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-'))
        {
            return Err(format!(
                "{ctx}: invalid key `{key}`: expected A-Z a-z 0-9 _ . : + -"
            ));
        }
    }
    if count > 50 {
        return Err(format!("{ctx}: expected at most 50 keys, got {count}"));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProcedureYaml {
    /// Internal identifier some tools write; leave it out when authoring by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Display name of the procedure, 1-100 characters.
    #[serde(deserialize_with = "trimmed_text")]
    pub name: String,

    /// Version label of the procedure (max 50 characters), e.g. "1.0.0". The
    /// procedure's own version, never the CLI's.
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "trimmed_text"
    )]
    pub version: String,

    /// Free text shown with the procedure.
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "trimmed_text"
    )]
    pub description: String,

    /// How slots and phases are scheduled, and what a first failure does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionConfig>,

    /// The identification form: which unit fields are asked, their defaults and
    /// constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitConfig>,

    /// Top level, NOT inside unit:. A run property: the operator's email or
    /// free-text name, recorded as the run's operated_by attribution (an email matching
    /// an organization member links their account; any other value is kept as a
    /// declared name). Same field shape as the unit fields; prompted on the
    /// identification screen alongside them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<UnitFieldConfig>,

    /// The run's operator screen: `ui: { panel: panels/run.html }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<ProcedureUiConfig>,

    /// Settings set outside the repository, per station or environment (a
    /// MES URL), by KEY (A-Z, 0-9, _; not TOFUPILOT_*). Read as {config.KEY}
    /// in with:, env: and unit defaults, config.KEY in a phase.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, ConfigSpecYaml>,

    /// Like config:, for credentials: write-only, masked in logs. Read as
    /// {secrets.KEY} and secrets.KEY.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretSpecYaml>,

    /// Environment variables of every process of the run. Text; markers
    /// as in with: except {run.slot.*}. A phase's or plug's env: wins.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_env"
    )]
    pub env: BTreeMap<String, String>,

    /// Instrument declarations. A phase receives a plug by naming the plug's key as
    /// a parameter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugs: Vec<PlugDefinitionYaml>,

    /// Phases run before main; per slot or once for all slots depending on their
    /// scope. An entry is a phase, or a sequence call (`uses:`) whose phases
    /// all take the call's scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<PhaseEntryYaml>,

    /// The test phases. At least one, or the procedure does not load. An entry
    /// is a phase, or a sequence call (`uses:`) that runs a sequence file's
    /// phases here, once per slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<PhaseEntryYaml>,

    /// Phases run after main, whatever main's outcome. An entry is a phase or a
    /// sequence call (`uses:`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<PhaseEntryYaml>,
}

impl ProcedureYaml {
    /// True when any list holds a sequence call: such a file needs the
    /// expansion step (`procedure::expand`) and its location on disk.
    pub fn has_sequence_calls(&self) -> bool {
        self.setup
            .iter()
            .chain(self.main.iter())
            .chain(self.teardown.iter())
            .any(|e| matches!(e, PhaseEntryYaml::Uses(_)))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub struct ProcedureDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    /// Optional label the user bumps by hand. Recorded with each run as
    /// the `procedure_version`. Empty when the YAML omits it, in which
    /// case the run is not linked to any version.
    #[serde(default)]
    #[validate(length(max = 50))]
    pub version: String,

    #[serde(default)]
    #[validate(length(max = 50000))]
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub execution: Option<ExecutionConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub unit: Option<UnitConfig>,

    /// Operated-by prompt config. A RUN property, not a unit field:
    /// the value is an email or free-text name forwarded to `runs.create` as
    /// `operated_by` (run attribution). Declared at the procedure
    /// root; when present, the identification screen prompts for it
    /// alongside the unit fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<UnitFieldConfig>,

    /// The run-level operator screen (`ui.panel` at the root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<ProcedureUiConfig>,

    /// The procedure's own `config:` declarations. The sequence files' are
    /// on their instances; `declared_values` merges both.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, ConfigSpecYaml>,

    /// The procedure's own `secrets:` declarations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretSpecYaml>,

    /// The procedure's `env:`, markers in place (expanded at run start).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    #[validate(nested)]
    pub plugs: Vec<PlugDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub setup: Vec<PhaseDefinition>,

    #[serde(default)]
    #[validate(nested)]
    pub main: Vec<PhaseDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub teardown: Vec<PhaseDefinition>,

    /// Every sequence call the expansion resolved, parents before children,
    /// in file order. The phases they brought in sit in `setup`, `main` and
    /// `teardown` with a `sequence_path`; this list is the tree over them:
    /// what the orchestrator scopes outcomes and cancellations by, what the
    /// upload reports as `sequences[]`, what Studio draws as groups. Empty
    /// for a procedure without `uses:`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<SequenceInstance>,
}

/// The error of converting a YAML that still holds sequence calls without
/// the expansion step. A `uses:` needs the file's location to be resolved,
/// which the pure conversion does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnexpandedSequenceCalls {
    pub references: Vec<String>,
}

impl std::fmt::Display for UnexpandedSequenceCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the procedure calls sequences ({}) and needs its file location to resolve them",
            self.references.join(", ")
        )
    }
}

impl TryFrom<ProcedureYaml> for ProcedureDefinition {
    type Error = UnexpandedSequenceCalls;

    /// The conversion of a procedure WITHOUT sequence calls. A file with
    /// `uses:` entries goes through `procedure::expand` instead, which
    /// reads the sequence files and produces the flat definition.
    fn try_from(raw: ProcedureYaml) -> Result<Self, Self::Error> {
        let references: Vec<String> = raw
            .setup
            .iter()
            .chain(raw.main.iter())
            .chain(raw.teardown.iter())
            .filter_map(|e| match e {
                PhaseEntryYaml::Uses(call) => Some(call.uses.clone()),
                PhaseEntryYaml::Phase(_) => None,
            })
            .collect();
        if !references.is_empty() {
            return Err(UnexpandedSequenceCalls { references });
        }
        let phases_of = |entries: Vec<PhaseEntryYaml>| -> Vec<PhaseDefinition> {
            entries
                .into_iter()
                .filter_map(|e| match e {
                    PhaseEntryYaml::Phase(p) => Some((*p).into()),
                    PhaseEntryYaml::Uses(_) => None,
                })
                .collect()
        };
        Ok(ProcedureDefinition {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            execution: raw.execution,
            unit: raw.unit,
            operated_by: raw.operated_by,
            ui: raw.ui,
            config: raw.config,
            secrets: raw.secrets,
            env: raw.env,
            plugs: raw.plugs.into_iter().map(|p| p.into()).collect(),
            setup: phases_of(raw.setup),
            main: phases_of(raw.main),
            teardown: phases_of(raw.teardown),
            sequences: Vec::new(),
        })
    }
}

/// The top-level list of the procedure YAML an entry lives in. Mirrors the
/// web's `EntryLocation.list` so a diagnostic's place resolves to a line
/// with the helper the Sequence view already trusts (`findListEntryLine`)
/// instead of by re-searching the message text in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefList {
    Plugs,
    Setup,
    Main,
    Teardown,
    /// A top-level key of the procedure (`ui.panel`), not a list entry:
    /// `index` is unused, `key` is the whole path.
    Root,
}

impl RefList {
    pub fn as_str(self) -> &'static str {
        match self {
            RefList::Plugs => "plugs",
            RefList::Setup => "setup",
            RefList::Main => "main",
            RefList::Teardown => "teardown",
            RefList::Root => "",
        }
    }
}

/// Where a lint finding points: an entry of a top-level list, by
/// position (the payload preserves file order, so this is the file's
/// index too), and optionally the key inside that entry (`python`,
/// `executable.command`, `config.<name>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLocation {
    pub list: RefList,
    pub index: usize,
    pub key: Option<String>,
}

impl RefLocation {
    pub fn entry(list: RefList, index: usize) -> Self {
        Self {
            list,
            index,
            key: None,
        }
    }
    pub fn key(list: RefList, index: usize, key: impl Into<String>) -> Self {
        Self {
            list,
            index,
            key: Some(key.into()),
        }
    }
}

/// `Error`: the run would fail or silently do the wrong thing — reported
/// by `validate`, refused by the runner. `Warning`: a likely mistake the
/// lint cannot be certain of — reported, never blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefSeverity {
    Error,
    Warning,
}

/// One finding of `resolve_runtime_refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefProblem {
    pub severity: RefSeverity,
    /// Stable machine key for the kind of finding (`plug_class_not_found`,
    /// `phase_missing_param`…). Never renamed: tools branch on it, the
    /// message is for people and may change.
    pub code: &'static str,
    pub message: String,
    pub location: RefLocation,
    /// The replacement for the YAML value at `location`, when the
    /// message's did-you-mean is not one by itself (`flash` for
    /// `phases/bench.py:flsh`). `None`: the lint reads it from the message.
    pub hint: Option<String>,
}

impl std::fmt::Display for RefProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl RefProblem {
    /// The message without its leading "Plug `x`: " / "Phase `x`: ", which
    /// the path in `render` already says.
    pub fn detail(&self) -> &str {
        // A phase a sequence call brought in is located at the call's
        // `uses:`, which does not name it: the message keeps it.
        if self.at_sequence_call() {
            return &self.message;
        }
        for entity in ["Plug `", "Phase `"] {
            if let Some((_, rest)) = self
                .message
                .strip_prefix(entity)
                .and_then(|rest| rest.split_once("`: "))
            {
                return rest;
            }
        }
        &self.message
    }

    /// Located at a sequence call's `uses:` rather than at the entry the
    /// problem is in, which sits in the sequence file.
    pub fn at_sequence_call(&self) -> bool {
        self.location.key.as_deref() == Some("uses")
    }

    /// Compiler-style line: `procedure.yaml:7:5: plugs[0].python: ...`,
    /// positioned with the procedure text's `positions`.
    pub fn render(&self, file: &str, positions: &super::yaml::Positions) -> String {
        let path = super::loader::location_path(&self.location);
        match positions.locate(&path) {
            Some((line, column)) => format!("{file}:{line}:{column}: {path}: {}", self.detail()),
            None => format!("{file}: {path}: {}", self.detail()),
        }
    }

    pub fn error(code: &'static str, message: impl Into<String>, location: RefLocation) -> Self {
        Self {
            severity: RefSeverity::Error,
            code,
            message: message.into(),
            location,
            hint: None,
        }
    }
    pub fn is_error(&self) -> bool {
        self.severity == RefSeverity::Error
    }
}

/// `{run.slot.key}` and `{run.slot.name}` (and their old spellings
/// `{slot}` and `{slot_name}`) in a unit `default_value`, replaced by the
/// slot key and display name. One pass, so a name that itself contains
/// `{slot}` is inserted as written: the load-time check and the identify
/// step must expand a default to the same text.
pub fn expand_slot_placeholders(value: &str, key: &str, name: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(after) = tail
            .strip_prefix("{slot_name}")
            .or_else(|| tail.strip_prefix("{run.slot.name}"))
        {
            out.push_str(name);
            rest = after;
        } else if let Some(after) = tail
            .strip_prefix("{slot}")
            .or_else(|| tail.strip_prefix("{run.slot.key}"))
        {
            out.push_str(key);
            rest = after;
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

impl ProcedureDefinition {
    /// Where a diagnostic about the expanded phase at `flat_index` of
    /// `list` points in the procedure file: the phase's own entry for a
    /// phase of the procedure, the `uses:` entry of its root call for a
    /// phase a call brought in. The editor counts the file's entries, and
    /// a call is one entry however many phases it expands to.
    pub fn written_location(&self, list: RefList, flat_index: usize, key: &str) -> RefLocation {
        let phases = match list {
            RefList::Setup => &self.setup,
            RefList::Main => &self.main,
            RefList::Teardown => &self.teardown,
            RefList::Plugs | RefList::Root => return RefLocation::key(list, flat_index, key),
        };
        let Some(phase) = phases.get(flat_index) else {
            return RefLocation::key(list, flat_index, key);
        };
        if let Some(top) = phase.sequence_path.first() {
            if let Some(call) = self
                .sequences
                .iter()
                .find(|s| s.path.len() == 1 && &s.path[0] == top)
            {
                return RefLocation::key(list, call.entry_index, "uses");
            }
        }
        let mut entries = 0;
        let mut last_top: Option<&str> = None;
        for p in &phases[..flat_index] {
            match p.sequence_path.first() {
                None => {
                    entries += 1;
                    last_top = None;
                }
                Some(t) if last_top != Some(t.as_str()) => {
                    entries += 1;
                    last_top = Some(t.as_str());
                }
                Some(_) => {}
            }
        }
        RefLocation::key(list, entries, key)
    }

    /// NOTE: no production code path writes this back to disk today —
    /// only tests round-trip it.
    pub fn to_yaml(&self) -> ProcedureYaml {
        ProcedureYaml {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            execution: self.execution.clone(),
            unit: self.unit.clone(),
            operated_by: self.operated_by.clone(),
            ui: self.ui.clone(),
            config: self.config.clone(),
            secrets: self.secrets.clone(),
            env: self.env.clone(),
            plugs: self.plugs.iter().map(|p| p.to_yaml()).collect(),
            setup: self.entries_to_yaml(&self.setup, PhaseStage::Setup),
            main: self.entries_to_yaml(&self.main, PhaseStage::Main),
            teardown: self.entries_to_yaml(&self.teardown, PhaseStage::Teardown),
        }
    }

    /// The YAML entries of one top-level list: its own phases as phases,
    /// and the sequence calls made from that list as `uses:` entries (the
    /// phases a call brought in are not written back, the call is). Calls
    /// come after the phases: the round trip keeps the content, not the
    /// interleaving, which no production path reads back anyway.
    fn entries_to_yaml(
        &self,
        phases: &[PhaseDefinition],
        stage: PhaseStage,
    ) -> Vec<PhaseEntryYaml> {
        let mut entries: Vec<PhaseEntryYaml> = phases
            .iter()
            .filter(|p| p.sequence_path.is_empty())
            .map(|p| PhaseEntryYaml::Phase(Box::new(p.to_yaml())))
            .collect();
        entries.extend(
            self.sequences
                .iter()
                .filter(|s| s.path.len() == 1 && s.stage == stage)
                .map(|s| PhaseEntryYaml::Uses(s.to_yaml())),
        );
        entries
    }
}

// Raw deserialization struct for SlotConfig
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct SlotConfigRaw {
    /// Slot key, a Python identifier; derived from name when omitted.
    #[serde(default)]
    key: Option<String>,
    /// Display name of the slot, 1-50 characters.
    name: String,
}

impl TryFrom<SlotConfigRaw> for SlotConfig {
    type Error = String;

    // A key that is not an identifier cannot be named from phase code
    // (`slot.key`), and it is a directory and log name on the station.
    fn try_from(raw: SlotConfigRaw) -> Result<Self, String> {
        let key = match raw.key {
            Some(user_key) if !crate::python::is_valid_python_identifier(&user_key) => {
                return Err(format!(
                    "key: invalid `{user_key}`: expected a Python identifier; fix or remove it"
                ));
            }
            Some(user_key) => user_key,
            None => crate::python::to_python_identifier(&raw.name),
        };
        Ok(SlotConfig {
            key,
            name: raw.name,
        })
    }
}

#[derive(Debug, Validate, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SlotConfig {
    #[validate(length(min = 1, max = 50))]
    pub key: String,

    #[validate(length(min = 1, max = 50))]
    pub name: String,
}

impl<'de> Deserialize<'de> for SlotConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = SlotConfigRaw::deserialize(deserializer)?;
        SlotConfig::try_from(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct UnitFieldConfig {
    /// Value pre-filled in the field. A non-empty default must itself satisfy
    /// pattern / min_length / max_length, locked or not: the procedure fails to
    /// load otherwise. A default holding {run.*}, {config.X} or {secrets.X} is
    /// exempt at load and checked after expansion, at identify time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,

    /// Placeholder text shown in the input field
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    /// Helper text shown below the field label (mirrors `UIComponent.description`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Minimum length for the field value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,

    /// Maximum length for the field value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,

    /// Regex the entry must match in FULL, so anchors are optional and change
    /// nothing: [A-Z0-9-]+ and ^[A-Z0-9-]+$ are the same rule; write the bare form.
    /// With prefix/suffix, it validates only what the operator types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    /// Error message shown when the value does not match `pattern`, with
    /// an example to copy ("A phone number, like 079-123-4567"). Write one
    /// when the pattern branches (alternation, groups, lookaheads): a plain
    /// one ([A-Z0-9-]+, SN-\d{4}) gets a derived per-character message
    /// ("Remove '#', allowed: …"), a branching one only "Doesn't match the
    /// required format".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_message: Option<String>,

    /// Fixed text prepended to the operator's input. The stored value is
    /// `prefix + input + suffix`; `min_length` / `max_length` / `pattern`
    /// validate the operator's typed input, before the affixes are
    /// composed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,

    /// Fixed text appended to the operator's input (see `prefix`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,

    /// The value is `default_value`, not a question: the identify form
    /// shows the field dimmed and read-only, and the engine fills it
    /// from this config, ignoring anything submitted under its key.
    /// Requires a non-empty `default_value` (`validate_field_default`,
    /// at load). When every field the form would show is locked the
    /// form is skipped, exactly like `auto_identify: true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

impl UnitFieldConfig {
    /// The value a locked field contributes without a prompt — the
    /// trimmed `default_value`, `None` when the field is not locked.
    /// A locked field with a blank default cannot exist past the
    /// loader (`validate_field_default`), so `None` means "not locked".
    pub fn locked_value(&self) -> Option<String> {
        if !self.locked {
            return None;
        }
        self.default_value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }
}

/// Whether `value` holds a marker resolved when the run starts (`{slot}`,
/// `{run.*}`, `{config.X}`, `{secrets.X}`): a default that is only known
/// then, and checked then.
pub(crate) fn has_run_start_marker(value: &str) -> bool {
    super::markers::find_markers(value)
        .iter()
        .any(|m| m.reference.is_run_start())
}

/// Load-time check of one field's `default_value` and `locked`. Any
/// non-empty default must pass the field's own `pattern` / `min_length`
/// / `max_length`, locked or not: an unlocked default that fails them
/// pre-fills the form with an entry the operator has to fix on every
/// run, and a locked one cannot be fixed at all — either way the typo
/// fails the deploy, not the first run. A locked field additionally
/// needs a non-empty default (nothing else can identify the unit). A
/// default carrying `{slot}` / `{slot_name}` is only checked for
/// presence: it is expanded per slot at identify time and validated
/// there, as the auto-identify path does today.
pub fn validate_field_default(ctx: &str, cfg: &UnitFieldConfig) -> Result<(), String> {
    let default = cfg
        .default_value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let Some(value) = default else {
        return if cfg.locked {
            Err(format!("{ctx}: `locked` needs `default_value`"))
        } else {
            Ok(())
        };
    };
    if has_run_start_marker(value) {
        return Ok(());
    }
    crate::unit::validate_unit_field(ctx, &Some(value.to_string()), cfg).map_err(|e| {
        // The field check names the field itself; the path says it already.
        let e = e.strip_prefix(&format!("{ctx}: ")).unwrap_or(&e);
        format!("{ctx}.default_value: {e}")
    })
}

/// Validate one field's `prefix` / `suffix` at procedure-load time.
/// `charset_restricted` marks unit-identity fields (serial / part /
/// revision / batch / sub-unit numbers) whose final value the server
/// rejects outside `[a-zA-Z0-9_.:+-]` — a bad affix there would only
/// fail at upload, after the test has run.
pub fn validate_unit_field_affixes(
    ctx: &str,
    cfg: &UnitFieldConfig,
    charset_restricted: bool,
) -> Result<(), String> {
    for (name, affix) in [
        ("prefix", cfg.prefix.as_deref()),
        ("suffix", cfg.suffix.as_deref()),
    ] {
        let Some(affix) = affix else { continue };
        if affix.is_empty() || affix.chars().count() > 50 {
            return Err(format!(
                "{ctx}.{name}: expected 1-50 characters, got {}",
                affix.chars().count()
            ));
        }
        if charset_restricted && !is_identity_charset(affix) {
            return Err(format!(
                "{ctx}.{name}: invalid `{affix}`: expected A-Z a-z 0-9 _ . : + -"
            ));
        }
    }
    Ok(())
}

/// The server's charset for unit identity values (`runs.create`) — the
/// same `UNIT_FIELD_CHARSET_PATTERN` the shared validator stamps on
/// pattern-less identity components, compiled the same way, so the two
/// cannot drift apart.
pub(crate) fn is_identity_charset(s: &str) -> bool {
    static CHARSET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    CHARSET
        .get_or_init(|| {
            station_protocol::pattern_messages::compile_field_pattern(
                station_protocol::pattern_messages::UNIT_FIELD_CHARSET_PATTERN,
            )
            .expect("UNIT_FIELD_CHARSET_PATTERN compiles")
        })
        .is_match(s)
}

/// Whether a phase component's `bind` lands in a unit identity field —
/// the ones `compose_bound_affixes` wraps and the server charset-checks.
/// `unit.metadata.*` and `run.*` are free text and stay out.
fn binds_unit_identity(bind: &str) -> bool {
    matches!(
        bind,
        "unit.serial_number" | "unit.part_number" | "unit.revision_number" | "unit.batch_number"
    ) || bind
        .strip_prefix("unit.sub_units.")
        .is_some_and(|k| !k.is_empty())
}

/// Whether an operator UI component of the phase records measurement
/// `key` through its `bind` (both spellings the worker accepts).
fn phase_binds_measurement(phase: &PhaseDefinition, key: &str) -> bool {
    phase
        .ui
        .iter()
        .flat_map(|u| u.components.iter().flatten())
        .filter_map(|c| c.bind.as_deref())
        .any(|bind| {
            bind.strip_prefix("measurements.")
                .or_else(|| bind.strip_prefix("measurement."))
                == Some(key)
        })
}

/// Configuration for a specific sub-unit with custom label and constraints
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SubUnitItemConfig {
    /// Display label for this sub-unit (e.g., "Battery", "Motor")
    #[serde(deserialize_with = "trimmed_text")]
    pub label: String,

    /// Internal key for this sub-unit (e.g., "battery", "motor")
    /// Used for binding, reports, and internal storage
    /// If not provided, auto-generated from label (lowercase, sanitized)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Optional serial number constraints for this specific sub-unit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<UnitFieldConfig>,
}

impl serde::Serialize for SubUnitItemConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SubUnitItemConfig", 3)?;
        state.serialize_field("label", &self.label)?;
        // Always serialize the computed key
        state.serialize_field("key", &self.get_key())?;
        state.serialize_field("serial_number", &self.serial_number)?;
        state.end()
    }
}

impl SubUnitItemConfig {
    /// Get the key for this sub-unit, auto-generating from label if not provided
    pub fn get_key(&self) -> String {
        self.key.clone().unwrap_or_else(|| {
            // Auto-generate key from label: lowercase and sanitize
            crate::python::to_python_identifier(&self.label.to_lowercase())
        })
    }
}

/// Configuration for sub-units collection (transparent wrapper for direct array serialization)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SubUnitsConfig(pub Vec<SubUnitItemConfig>);

impl validator::Validate for SubUnitsConfig {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        validate_sub_units_config(self).map_err(|e| {
            let mut errors = validator::ValidationErrors::new();
            // The list itself: the loader names it by its own path.
            errors.add("__all__", e);
            errors
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, Validate)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UnitConfig {
    /// When true, automatically submit unit identification using default_value fields.
    /// Requires serial_number.default_value and part_number.default_value to be set,
    /// and a serial_number.default_value on every declared sub-unit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_identify: bool,

    /// Serial number field configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<UnitFieldConfig>,

    /// Part number field configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<UnitFieldConfig>,

    /// Revision number field configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_number: Option<UnitFieldConfig>,

    /// Batch number field configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_number: Option<UnitFieldConfig>,

    /// Sub-units configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub sub_units: Option<SubUnitsConfig>,

    /// Operator-prompted unit metadata fields, keyed by metadata key.
    /// Each field renders as a text input in the identify form (alphabetical
    /// order) and its value is upserted onto the unit at upload. Values are
    /// always optional — blank input means the key is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<std::collections::BTreeMap<String, UnitFieldConfig>>,
}

impl UnitConfig {
    /// Validates that auto_identify requirements are met:
    /// serial_number and part_number must both have a `default_value`,
    /// and every configured sub-unit must have a
    /// `serial_number.default_value`. Without the sub-unit check the
    /// runtime would silently drop missing-default sub-units and the
    /// identify step would fail with an opaque "expected N got M"
    /// from `validate_unit_info` rather than naming the offender.
    pub fn validate_auto_identify(&self) -> Result<(), String> {
        if !self.auto_identify {
            return Ok(());
        }

        let sn_ok = self
            .serial_number
            .as_ref()
            .and_then(|f| f.default_value.as_ref())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        let pn_ok = self
            .part_number
            .as_ref()
            .and_then(|f| f.default_value.as_ref())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        if !sn_ok {
            return Err("unit.auto_identify: needs `unit.serial_number.default_value`".to_string());
        }
        if !pn_ok {
            return Err("unit.auto_identify: needs `unit.part_number.default_value`".to_string());
        }

        if let Some(sub_units) = self.sub_units.as_ref() {
            for item in &sub_units.0 {
                let has_default = item
                    .serial_number
                    .as_ref()
                    .and_then(|f| f.default_value.as_ref())
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                if !has_default {
                    return Err(format!(
                        "unit.auto_identify: needs `unit.sub_units.{}.serial_number.default_value`",
                        item.get_key()
                    ));
                }
            }
        }

        Ok(())
    }

    /// Load-time check of every field's `prefix` / `suffix` (length and,
    /// for identity fields, the server charset). The root `operated_by:`
    /// config is validated separately by the loader — it lives outside
    /// this struct.
    pub fn validate_affixes(&self) -> Result<(), String> {
        let identity_fields = [
            ("unit.serial_number", self.serial_number.as_ref()),
            ("unit.part_number", self.part_number.as_ref()),
            ("unit.revision_number", self.revision_number.as_ref()),
            ("unit.batch_number", self.batch_number.as_ref()),
        ];
        for (ctx, cfg) in identity_fields {
            if let Some(cfg) = cfg {
                validate_unit_field_affixes(ctx, cfg, true)?;
            }
        }
        if let Some(sub_units) = self.sub_units.as_ref() {
            for (index, item) in sub_units.0.iter().enumerate() {
                if let Some(cfg) = item.serial_number.as_ref() {
                    validate_unit_field_affixes(
                        &format!("unit.sub_units[{index}].serial_number"),
                        cfg,
                        true,
                    )?;
                }
            }
        }
        // Metadata values are free text server-side — length check only.
        if let Some(md) = self.metadata.as_ref() {
            for (key, cfg) in md {
                validate_unit_field_affixes(&format!("unit.metadata.{key}"), cfg, false)?;
            }
        }
        Ok(())
    }

    /// Load-time check of every field's `default_value` and `locked`
    /// (see `validate_field_default`). The root `operated_by:` is
    /// checked by the loader — it lives outside this struct.
    pub fn validate_defaults(&self) -> Result<(), String> {
        for (ctx, cfg) in self.named_fields() {
            validate_field_default(&ctx, cfg)?;
        }
        Ok(())
    }

    /// A default that is uploaded as-is (the field is `locked`, or the
    /// procedure is `auto_identify`) must meet the server's rules on unit
    /// identity values once composed with its prefix and suffix, for
    /// every slot when it carries `{slot}` / `{slot_name}`. An unlocked
    /// default is a pre-fill the operator overwrites and stays exempt.
    pub fn validate_uploaded_defaults(&self, slots: &[SlotConfig]) -> Result<(), String> {
        // (YAML path, field name the server's rule is worded with, config)
        let mut identity: Vec<(String, String, &UnitFieldConfig)> = Vec::new();
        for (field, cfg) in [
            ("serial_number", self.serial_number.as_ref()),
            ("part_number", self.part_number.as_ref()),
            ("revision_number", self.revision_number.as_ref()),
            ("batch_number", self.batch_number.as_ref()),
        ] {
            if let Some(cfg) = cfg {
                identity.push((format!("unit.{field}"), field.to_string(), cfg));
            }
        }
        if let Some(sub_units) = self.sub_units.as_ref() {
            for (index, item) in sub_units.0.iter().enumerate() {
                if let Some(cfg) = item.serial_number.as_ref() {
                    identity.push((
                        format!("unit.sub_units[{index}].serial_number"),
                        format!("{} serial number", item.label),
                        cfg,
                    ));
                }
            }
        }
        for (path, field, cfg) in identity {
            if !(cfg.locked || self.auto_identify) {
                continue;
            }
            let Some(default) = cfg
                .default_value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            else {
                continue;
            };
            let compose = |value: &str| {
                format!(
                    "{}{}{}",
                    cfg.prefix.as_deref().unwrap_or(""),
                    value,
                    cfg.suffix.as_deref().unwrap_or("")
                )
            };
            let markers = super::markers::find_markers(default);
            // A config value, a secret or the station is only known when the run
            // starts: the identify step checks the expanded value then.
            if markers
                .iter()
                .any(|m| m.reference.is_run_start() && !m.reference.reads_slot())
            {
                continue;
            }
            // An unknown `{run.slot.*}` field is reported on its own (with
            // its did-you-mean); expanding it would only add a side effect.
            if markers.iter().any(|m| match &m.reference {
                super::markers::MarkerRef::Namespaced { namespace, field } => {
                    namespace == "run" && !super::markers::RUN_FIELDS.contains(&field.as_str())
                }
                _ => false,
            }) {
                continue;
            }
            let has_placeholder = markers.iter().any(|m| m.reference.reads_slot());
            if !has_placeholder {
                crate::unit::validate_identity_wire_limits(&field, &compose(default))
                    .map_err(|e| format!("{path}.default_value: {e}"))?;
                continue;
            }
            for slot in slots {
                let expanded = expand_slot_placeholders(default, &slot.key, &slot.name);
                crate::unit::validate_identity_wire_limits(&field, &compose(&expanded)).map_err(
                    |e| {
                        format!(
                            "{path}.default_value: expands to `{expanded}` for slot {}: {e}",
                            slot.key
                        )
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Whether the identify form has anything for the operator to type:
    /// false when every component it would show is locked, which is the
    /// per-field spelling of `auto_identify: true`. Serial and part are
    /// always shown, so an undeclared one still prompts; revision, batch,
    /// `operated_by`, sub-units and metadata only when declared. Mirrors
    /// `identify_unit::components::build_components` — a component the
    /// builder emits must be counted here or a locked-only form would be
    /// shown with nothing to fill.
    pub fn prompts_operator(&self, operated_by: Option<&UnitFieldConfig>) -> bool {
        let unlocked = |cfg: Option<&UnitFieldConfig>| cfg.is_none_or(|f| !f.locked);
        if unlocked(self.serial_number.as_ref()) || unlocked(self.part_number.as_ref()) {
            return true;
        }
        if self.revision_number.as_ref().is_some_and(|f| !f.locked)
            || self.batch_number.as_ref().is_some_and(|f| !f.locked)
            || operated_by.is_some_and(|f| !f.locked)
        {
            return true;
        }
        if let Some(sub_units) = self.sub_units.as_ref() {
            if sub_units
                .0
                .iter()
                .any(|item| unlocked(item.serial_number.as_ref()))
            {
                return true;
            }
        }
        if let Some(md) = self.metadata.as_ref() {
            if md.values().any(|f| !f.locked) {
                return true;
            }
        }
        false
    }

    /// Every declared field, mutably: what expands a `default_value`.
    pub fn for_each_field_mut(&mut self, mut f: impl FnMut(&mut UnitFieldConfig)) {
        for field in [
            &mut self.serial_number,
            &mut self.part_number,
            &mut self.revision_number,
            &mut self.batch_number,
        ]
        .into_iter()
        .flatten()
        {
            f(field);
        }
        if let Some(sub) = self.sub_units.as_mut() {
            for item in sub.0.iter_mut().filter_map(|i| i.serial_number.as_mut()) {
                f(item);
            }
        }
        if let Some(md) = self.metadata.as_mut() {
            for field in md.values_mut() {
                f(field);
            }
        }
    }

    /// Every declared field with its YAML path, for checks that treat
    /// them alike (`validate_defaults`).
    pub(crate) fn named_fields(&self) -> Vec<(String, &UnitFieldConfig)> {
        let mut out: Vec<(String, &UnitFieldConfig)> = Vec::new();
        for (ctx, cfg) in [
            ("unit.serial_number", self.serial_number.as_ref()),
            ("unit.part_number", self.part_number.as_ref()),
            ("unit.revision_number", self.revision_number.as_ref()),
            ("unit.batch_number", self.batch_number.as_ref()),
        ] {
            if let Some(cfg) = cfg {
                out.push((ctx.to_string(), cfg));
            }
        }
        if let Some(sub_units) = self.sub_units.as_ref() {
            for (index, item) in sub_units.0.iter().enumerate() {
                if let Some(cfg) = item.serial_number.as_ref() {
                    out.push((format!("unit.sub_units[{index}].serial_number"), cfg));
                }
            }
        }
        if let Some(md) = self.metadata.as_ref() {
            for (key, cfg) in md {
                out.push((format!("unit.metadata.{key}"), cfg));
            }
        }
        out
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// Order of work across slots: phase_first runs each phase on every slot before
    /// the next phase, slot_first runs a whole slot before the next. Default
    /// phase_first.
    #[serde(default = "default_strategy")]
    pub strategy: ExecutionStrategy,

    /// Parallel workers, 1-256. Default 8.
    #[serde(default = "default_workers")]
    #[validate(range(min = 1, max = 256))]
    pub workers: usize,

    /// What the run does when a phase fails: stop (skip the remaining phases, run
    /// teardown) or continue. A phase's own then: overrides it. Default stop.
    #[serde(default = "default_stop")]
    pub on_first_failure: FirstFailureAction,

    /// Multi-slot stations only. An integer 1-1024 expands to keys slot_1..slot_N
    /// (zero-padded: 12 slots -> slot_01..slot_12), or a list of {name, key?};
    /// slots x phases <= 10000. Unit and operated_by defaults may use
    /// {run.slot.key} / {run.slot.name}. Per-slot serials come only from
    /// serial_number.default_value with {run.slot.key} ("BURNIN-{run.slot.key}"),
    /// never from a phase: identification runs before any phase.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_slots"
    )]
    #[validate(nested)]
    #[cfg_attr(feature = "json-schema", schemars(with = "SlotsSchema"))]
    pub slots: Vec<SlotConfig>,
}

/// Cap on the `slots: N` integer shorthand: a typo must not spawn a job
/// explosion. Explicit lists are uncapped, one line per slot is its own
/// brake. The engine still bounds slots x phases at its job queue limit.
const MAX_SLOT_COUNT_SHORTHAND: u64 = 1024;

/// `slots:` accepts the explicit list or an integer shorthand
/// (`slots: 80`), like GitLab's `parallel: N`. The shorthand expands to
/// `slot_01..slot_NN` with ordinals zero-padded to the count, so
/// lexicographic order equals numeric order everywhere downstream.
/// A visitor rather than an untagged enum, so a bad list entry still
/// reports its own error instead of "did not match any variant".
fn deserialize_slots<'de, D>(deserializer: D) -> Result<Vec<SlotConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    struct SlotsVisitor;

    impl<'de> serde::de::Visitor<'de> for SlotsVisitor {
        type Value = Vec<SlotConfig>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a slot count or a list of slots")
        }

        fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Self::Value, E> {
            if n == 0 {
                return Err(E::custom("expected at least 1, got 0"));
            }
            if n > MAX_SLOT_COUNT_SHORTHAND {
                return Err(E::custom(format!(
                    "expected at most {MAX_SLOT_COUNT_SHORTHAND}, got {n}"
                )));
            }
            let width = n.to_string().len();
            Ok((1..=n)
                .map(|i| SlotConfig {
                    key: format!("slot_{i:0width$}"),
                    name: format!("Slot {i}"),
                })
                .collect())
        }

        fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Self::Value, E> {
            if n < 1 {
                return Err(E::custom(format!("expected at least 1, got {n}")));
            }
            self.visit_u64(n as u64)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Vec::<SlotConfig>::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(SlotsVisitor)
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            strategy: ExecutionStrategy::PhaseFirst,
            workers: 8,
            on_first_failure: FirstFailureAction::Stop,
            slots: vec![],
        }
    }
}

impl ExecutionConfig {
    /// Validate consistency between global execution config and phase-level then configs
    /// Returns warnings for potentially conflicting configurations
    pub fn validate_consistency(&self, phases: &[&PhaseDefinition]) -> Vec<String> {
        let mut warnings = Vec::new();

        for phase in phases {
            if let Some(then_config) = &phase.then {
                // Warning: on_first_failure: stop + then.fail: continue (conflict)
                if matches!(self.on_first_failure, FirstFailureAction::Stop) {
                    if let Some(PhaseNextAction::Continue) = then_config.fail {
                        warnings.push(format!(
                            "Phase {}: `then.fail: continue` overrides `execution.on_first_failure: stop`",
                            phase.key
                        ));
                    }
                }
            }
        }

        warnings
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct RetryConfig {
    #[validate(range(min = 1, max = 10))]
    pub limit: usize,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    #[validate(range(max = 3600000))]
    pub delay: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RetryConfigYaml {
    /// Retries after the first attempt, 1-10 (limit: 3 = 4 attempts in total). The
    /// field is limit, never max.
    #[cfg_attr(feature = "json-schema", schemars(range(min = 1, max = 10)))]
    pub limit: usize,

    /// Pause between attempts, a duration like 2s, at most 1h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration_as_string"
    )]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<DurationText>"))]
    pub delay: Option<u64>,
}

impl From<RetryConfigYaml> for RetryConfig {
    fn from(yaml: RetryConfigYaml) -> Self {
        RetryConfig {
            limit: yaml.limit,
            delay: yaml.delay,
        }
    }
}

impl From<&RetryConfig> for RetryConfigYaml {
    fn from(config: &RetryConfig) -> Self {
        RetryConfigYaml {
            limit: config.limit,
            delay: config.delay,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum PhaseNextAction {
    Continue,
    Stop,
    Retry,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ThenConfig {
    /// Action when the phase passes: continue (default), stop or retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<PhaseNextAction>,

    /// Action when a measurement fails its validators: continue, stop or retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail: Option<PhaseNextAction>,

    /// Action when the phase raises: continue, stop or retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PhaseNextAction>,

    /// Action when the phase exceeds its timeout: continue, stop or retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<PhaseNextAction>,
}

// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct PlugDefinitionYaml {
    /// The parameter name a phase uses to receive this plug; lowercased and
    /// underscored from name when absent ("Power Supply" -> power_supply). Unique
    /// across plugs, case-insensitively. Write it explicitly when the name changes
    /// and phases must keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Display name of the instrument.
    #[serde(deserialize_with = "trimmed_text")]
    pub name: String,
    /// Lifetime of the instrument instance: slot (one per slot, the default),
    /// execution (one shared by every slot of an execution) or station (held
    /// across executions, only when a station plug host is present, i.e. in
    /// station mode; otherwise it behaves like execution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// "file:ClassName" reference to the plug class, e.g.
    /// plugs.power_supply:PowerSupply -> class PowerSupply in
    /// plugs/power_supply.py. Required.
    pub python: PythonSpec,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "trimmed_text"
    )]
    pub description: String,
    /// Keyword arguments of the plug class __init__: with: {address: "10.0.0.5"}
    /// -> PowerSupply(address="10.0.0.5"); keys must match its signature. Two
    /// instruments of one model: the class twice, different keys and with:.
    /// Values may hold {config.X}, {secrets.X} and {run.*} markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<serde_json::Map<String, serde_json::Value>>,
    /// Old spelling of with:, deprecated (not the procedure's top-level config: block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// Environment variables of the plug's process, markers as in with:.
    /// Not on a station-scoped plug.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_env"
    )]
    pub env: BTreeMap<String, String>,
    /// Time the plug's __init__ may take to open the instrument: default
    /// 30s, at most 1h.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration_as_string"
    )]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<DurationText>"))]
    pub init_timeout: Option<u64>,
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlugDefinition {
    #[validate(
        length(min = 1, max = 100),
        custom(function = "validate_python_identifier")
    )]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    pub scope: Scope,

    pub python: PythonSpec,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// Keyword arguments passed to the plug class `__init__` at
    /// instantiation, markers in place (expanded at run start). Serialized
    /// as `config`, the key every consumer of the loaded definition reads.
    #[serde(rename = "config", default, skip_serializing_if = "Option::is_none")]
    pub with: Option<serde_json::Map<String, serde_json::Value>>,

    /// The plug's own `env:`, markers in place.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// `init_timeout:` in milliseconds; absent, the engine's default.
    #[validate(range(min = 1, max = 3600000))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_timeout: Option<u64>,

    /// The file wrote `config:`, the old spelling of `with:`: diagnostics
    /// name the key the author wrote. Never serialized.
    #[serde(skip)]
    pub with_written_as_config: bool,

    /// The file wrote both `with:` and `config:`; the loader refuses it.
    #[serde(skip)]
    pub with_written_twice: bool,
}

impl From<PlugDefinitionYaml> for PlugDefinition {
    fn from(yaml: PlugDefinitionYaml) -> Self {
        let key = yaml
            .key
            .unwrap_or_else(|| crate::python::to_python_identifier(&yaml.name));

        PlugDefinition {
            key,
            name: yaml.name,
            scope: yaml.scope.unwrap_or(Scope::Slot),
            python: yaml.python,
            with_written_as_config: yaml.with.is_none() && yaml.config.is_some(),
            with_written_twice: yaml.with.is_some() && yaml.config.is_some(),
            description: yaml.description,
            with: yaml.with.or(yaml.config),
            env: yaml.env,
            init_timeout: yaml.init_timeout,
        }
    }
}

impl PlugDefinition {
    pub fn to_yaml(&self) -> PlugDefinitionYaml {
        let auto_key = crate::python::to_python_identifier(&self.name);
        PlugDefinitionYaml {
            key: if self.key != auto_key {
                Some(self.key.clone())
            } else {
                None
            },
            name: self.name.clone(),
            // Omit the default so procedures that never specified a scope
            // stay byte-identical on round-trip (Studio re-saves whole
            // files).
            scope: if self.scope == Scope::Slot {
                None
            } else {
                Some(self.scope)
            },
            python: self.python.clone(),
            description: self.description.clone(),
            with: self.with.clone().filter(|_| !self.with_written_as_config),
            config: self.with.clone().filter(|_| self.with_written_as_config),
            env: self.env.clone(),
            init_timeout: self.init_timeout,
        }
    }
}

impl<'de> Deserialize<'de> for PlugDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = PlugDefinitionYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

impl PlugDefinition {
    /// The plug service's spawn config, `with:` as written (markers in
    /// place). `config` is the key tp_plug.py reads the keyword arguments
    /// under.
    pub fn to_config_json(&self, project_dir: &Path) -> Result<serde_json::Value, String> {
        let mut config = match self.python.parse(project_dir) {
            Ok((file_path, callable_name)) => serde_json::json!({
                "file": file_path.to_string_lossy(),
                "class": callable_name,
                "config": self.with,
            }),
            // Not in the procedure tree: a class of an installed package
            // (a vendor driver), imported by module name, as a phase is.
            Err(e) => match self.python.installed_module(project_dir) {
                Some((module, class)) => serde_json::json!({
                    "module": module,
                    "class": class,
                    "config": self.with,
                }),
                None => return Err(e),
            },
        };
        // Read by the plug service manager, which waits this long for the
        // service to report ready; never handed to the class.
        if let Some(ms) = self.init_timeout {
            config["init_timeout_ms"] = ms.into();
        }
        Ok(config)
    }

    /// The spawn config of one instance, run-start markers expanded with
    /// `values` for `slot` (`None` for an execution or station plug). The
    /// plug's own `env:` rides along under `env`, which the plug service
    /// manager sets on the process and never hands to the class.
    pub fn to_resolved_config_json(
        &self,
        project_dir: &Path,
        values: &super::values::RunValues,
        slot: Option<&super::values::SlotContext>,
    ) -> Result<serde_json::Value, String> {
        let mut config = self.to_config_json(project_dir)?;
        if let Some(with) = &self.with {
            let with = serde_json::Value::Object(with.clone());
            config["config"] = if self.with_written_as_config {
                values.expand_config(&with, slot)
            } else {
                values.expand(&with, slot)
            };
        }
        if !self.env.is_empty() {
            let env: serde_json::Map<String, serde_json::Value> = values
                .expand_env(&self.env, slot)
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            config["env"] = serde_json::Value::Object(env);
        }
        Ok(config)
    }

    /// `with` or `config`: the key the file wrote the keyword arguments
    /// under, for diagnostics.
    pub fn with_key(&self) -> &'static str {
        if self.with_written_as_config {
            "config"
        } else {
            "with"
        }
    }

    /// True for execution-scope plugs: shared across all slots of one run.
    pub fn scope_is_execution(&self) -> bool {
        matches!(self.scope, Scope::Execution)
    }

    /// True for station-scope plugs: held across runs by the station
    /// plug host.
    pub fn scope_is_station(&self) -> bool {
        matches!(self.scope, Scope::Station)
    }
}

/// `path` relative to `dir` when it is under it, as given otherwise:
/// messages name `plugs/dmm.py`, not the absolute path.
fn relative_to(path: &Path, dir: &Path) -> String {
    // File-path specs are canonicalized, module specs are joined as given.
    let canonical = crate::path_utils::canonicalize_for_spawn(dir).ok();
    std::iter::once(dir)
        .chain(canonical.as_deref())
        .find_map(|d| path.strip_prefix(d).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Python specification: "path/to/file:callable_name" or "path.to.file:callable_name"
///
/// Examples:
/// - "phases/test_voltage" → phases/test_voltage.py::test_voltage()
/// - "phases.calibration:setup" → phases/calibration.py::setup()
/// - "plugs.python.ethercat:EtherCATManager" → plugs/python/ethercat.py::EtherCATManager
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PythonSpec(String);

impl PythonSpec {
    /// A spec from its raw text, as the expansion rewrites one into the
    /// procedure directory's frame of reference.
    pub fn new(spec: impl Into<String>) -> Self {
        PythonSpec(spec.into())
    }

    /// Raw "module.callable" reference as written in the YAML.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve the spec to (file_path, callable_name) WITHOUT requiring
    /// the file on disk. Split from `parse` so `resolve_runtime_refs` can
    /// decide what a missing file means (a tree-bound spec is broken, a
    /// bare dot-syntax one may still import from site-packages) without
    /// duplicating the grammar.
    pub fn resolve_path(&self, project_dir: &Path) -> Result<(PathBuf, String), String> {
        let spec = self.0.trim();

        // Validate not empty
        if spec.is_empty() {
            return Err("empty".into());
        }

        // Split on ':' to separate path from callable name
        let parts: Vec<&str> = spec.split(':').collect();

        // Handle Windows absolute paths (e.g., "C:/path/to/file.py:func")
        // Windows paths have a drive letter followed by colon, so we may have 3 parts
        let (path_part, name_part) = if parts.len() == 3
            && parts[0].len() == 1
            && parts[0]
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic())
                .unwrap_or(false)
        {
            // Windows absolute path: "C:/path/file.py:func" -> ["C", "/path/file.py", "func"]
            let path = format!("{}:{}", parts[0], parts[1]);
            (path, Some(parts[2].trim()))
        } else if parts.len() > 2 {
            return Err(format!("invalid `{spec}`: expected one `:`"));
        } else {
            (parts[0].trim().to_string(), parts.get(1).map(|s| s.trim()))
        };

        // Validate path not empty
        if path_part.is_empty() {
            return Err("missing module before `:`".into());
        }

        // Check if path uses slash syntax (file path) or dot syntax (module path)
        let is_file_path = path_part.contains('/') || path_part.contains('\\');

        let file_path = if is_file_path {
            // File path syntax: resolve relative to project_dir, allow ..
            let expanded_path = if let Some(rest) = path_part.strip_prefix('~') {
                if let Some(home) = dirs::home_dir() {
                    home.join(rest.trim_start_matches('/'))
                } else {
                    return Err("cannot expand `~`: no home directory".into());
                }
            } else {
                project_dir.join(&path_part)
            };

            // Canonicalize to resolve .. and symlinks. Strip `\\?\`
            // on Windows so the path is safe to use in spawn argv.
            let resolved =
                crate::path_utils::canonicalize_for_spawn(&expanded_path).unwrap_or(expanded_path);

            // Ensure .py extension; a folder is its package's `__init__.py`.
            if resolved.extension().map(|e| e == "py").unwrap_or(false) {
                resolved
            } else if resolved.join("__init__.py").is_file() {
                resolved.join("__init__.py")
            } else {
                resolved.with_extension("py")
            }
        } else {
            // Module path syntax (dots): no .. allowed, resolve relative to project_dir
            if path_part.contains("..") {
                return Err(format!(
                    "`..` not allowed in module path `{path_part}`; use a file path, e.g. `../dir/file.py:name`"
                ));
            }

            // Convert "plugs.python.ethercat" → "plugs/python/ethercat.py",
            // or a package's "plugs/python/ethercat/__init__.py".
            let module = project_dir.join(path_part.replace('.', "/"));
            let file = module.with_extension("py");
            let package = module.join("__init__.py");
            if !file.exists() && package.is_file() {
                package
            } else {
                file
            }
        };

        // Get callable name (or default to last component of path)
        let callable_name = if let Some(name) = name_part {
            if name.is_empty() {
                return Err("missing name after `:`".into());
            }
            name.to_string()
        } else {
            // Default to file stem (filename without .py); a package's
            // folder name.
            let named = if file_path.file_name().is_some_and(|n| n == "__init__.py") {
                file_path.parent().unwrap_or(&file_path)
            } else {
                &file_path
            };
            named
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&path_part)
                .to_string()
        };

        Ok((file_path, callable_name))
    }

    /// Parse the spec and resolve to (file_path, callable_name)
    ///
    /// # Arguments
    /// * `project_dir` - Root directory of the project
    ///
    /// # Returns
    /// * `Ok((PathBuf, String))` - Resolved file path and callable name (function or class)
    /// * `Err(String)` - Validation or resolution error
    pub fn parse(&self, project_dir: &Path) -> Result<(PathBuf, String), String> {
        let (file_path, callable_name) = self.resolve_path(project_dir)?;

        // Validate file exists
        if !file_path.exists() {
            return Err(format!(
                "Python file not found: {}",
                relative_to(&file_path, project_dir)
            ));
        }

        // Validate callable name is valid Python identifier
        if !crate::python::is_valid_python_identifier(&callable_name) {
            return Err(format!(
                "invalid `{callable_name}`: expected a Python identifier"
            ));
        }

        Ok((file_path, callable_name))
    }

    /// True when the spec can only resolve inside the procedure tree:
    /// file-path syntax (`/` or `\`), a `~` or leading-dot prefix, or dot
    /// syntax whose first segment exists as a directory in the tree — the
    /// procedure dir is on the worker's sys.path, so such an import binds
    /// in-tree and a missing file is authoritative. A bare dot-syntax spec
    /// with no matching in-tree directory may still resolve through
    /// tp_worker's importlib fallback (workspace wheels in site-packages),
    /// which cannot be checked statically. That includes a single-segment
    /// spec (`python: check`): with no dot there is no parent directory to
    /// probe, so unless a directory of the same name happens to exist it is
    /// never tree-bound and a typo there is left to the runtime.
    pub fn is_tree_bound(&self, project_dir: &Path) -> bool {
        let spec = self.0.trim();
        let path_part = spec.split(':').next().unwrap_or(spec).trim();
        if path_part.contains('/')
            || path_part.contains('\\')
            || path_part.starts_with('~')
            || path_part.starts_with('.')
        {
            return true;
        }
        match path_part.split('.').next() {
            Some(first) if !first.is_empty() => project_dir.join(first).is_dir(),
            _ => true,
        }
    }

    /// `(module, callable)` for a dotted spec with no file in the tree
    /// that may name an installed package (`acme_drivers.psu:PSU`): the
    /// runtime imports it by module name. `None` for a tree-bound spec or
    /// one whose file exists.
    pub fn installed_module(&self, project_dir: &Path) -> Option<(String, String)> {
        let (file, callable) = self.resolve_path(project_dir).ok()?;
        // `bad.py:Bad` is a file's spelling, never a module's.
        let module = self.get_module();
        (!file.exists()
            && !module.ends_with(".py")
            && !self.is_tree_bound(project_dir)
            && crate::python::is_valid_python_identifier(&callable))
        .then(|| (self.get_module(), callable))
    }

    /// Get the callable name (for display purposes, doesn't validate)
    pub fn get_callable_name(&self) -> String {
        let spec = self.0.trim();
        if let Some(name) = spec.split(':').nth(1) {
            name.trim().to_string()
        } else {
            spec.rsplit(&['.', '/'][..])
                .next()
                .unwrap_or(spec)
                .to_string()
        }
    }

    /// Get the module path (for backward compat with execution code)
    pub fn get_module(&self) -> String {
        let spec = self.0.trim();
        spec.split(':').next().unwrap_or(spec).to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExecutableConfig {
    /// The command to run. An empty string loads, but validation reports it and a
    /// run refuses to start: it means "a command runtime, command not chosen yet".
    //
    // `max` only, deliberately. It used to be `min = 1`, which meant an
    // editor could not write "this phase runs a command, which is not
    // chosen yet" — the only file it could produce was one that fails to
    // load, taking the whole procedure (and Studio's Sequence view, which
    // runs the same loader) down with the edit that created it. Emptiness
    // is caught by `resolve_runtime_refs`, which `validate` reports and the
    // runner refuses to start on — the same treatment a dangling `python:`
    // ref gets, and for the same reason.
    #[validate(length(max = 10000))]
    pub command: String,

    /// Shell that runs the command: bash, sh, zsh, powershell, pwsh or cmd.
    /// Omitted: powershell on Windows, sh elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub shell: Option<String>,

    /// Working directory of the command, relative to the procedure directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 4096))]
    pub working_directory: Option<String>,
}

// Raw deserialization struct for PhaseDefinition
// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct PhaseDefinitionYaml {
    /// Phase key, a Python identifier; derived from name when omitted. Other phases
    /// name it in depends_on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Display name of the phase.
    #[serde(deserialize_with = "trimmed_text")]
    pub name: String,
    /// On a setup or teardown phase: slot (runs per slot, the default) or execution
    /// (runs once for all slots). Never station, which is a plug lifetime and is
    /// rejected at load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// "file:function" reference to the phase function, e.g. phases.main:check ->
    /// def check() in phases/main.py, resolved relative to the directory holding
    /// this procedure.yaml. A phase takes python OR executable, never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSpec>,
    /// Runs a shell command instead of Python. A phase with neither python nor
    /// executable is valid: it runs no code, only its UI and measurements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableConfig>,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
    /// The values this phase records; their validators decide pass or fail.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "judgeable_measurements"
    )]
    pub measurements: Vec<MeasurementSpecYaml>,
    /// The operator screen shown while the phase runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UIConfig>,
    /// false skips the phase entirely. Default true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Reserved: "skip" is an older spelling of enabled: false. Leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Keys of phases this one waits for. Ordering only: a dependency that failed
    /// does not skip this phase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Wall-clock limit for one attempt, a duration like 30s, at most 24h.
    /// Absent means unlimited.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration_as_string"
    )]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<DurationText>"))]
    pub timeout: Option<u64>,
    /// Retry budget for this phase (limit, delay). A retry happens when then:
    /// names retry for an outcome, or when the phase code calls
    /// phase.retry(); this sets the budget for both. Absent, the budget is
    /// 3 retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfigYaml>,
    /// What happens after this phase, per outcome (pass, fail, error, timeout):
    /// continue, stop or retry. "then: fail: retry" re-runs the phase when a
    /// measurement fails, with no code (an operator choosing "retake", for
    /// instance); phase.retry() in the phase code is the other way. Without
    /// a retry: block the budget is 3 retries. Overrides
    /// execution.on_first_failure for this phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,
    /// Keyword arguments of the phase function: with: {channel: 2} calls it
    /// with channel=2. Keys must be its parameters, never an injected one, a
    /// plug key or a phase key. Markers as in a plug's with:.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, serde_json::Value>,
    /// Environment variables while this phase runs, markers as in with:.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_env"
    )]
    pub env: BTreeMap<String, String>,
}

/// A sequence call: an entry of setup, main or teardown that runs the
/// phases of a sequence file here, instead of one phase. The file's
/// phases run with their own setup, main and teardown barriers, take the
/// scope of this entry, and report one outcome for the whole call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct SequenceCallYaml {
    /// Display name of this call; the sequence file's own name when omitted.
    /// Shown as the sequence's name in the run.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub name: Option<String>,
    /// Instance key of this call, a Python identifier; derived from name when
    /// omitted. Prefixes the keys of the phases it brings in
    /// (`<key>/<phase key>`), and other entries name it in depends_on. Call
    /// the same file twice with two keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The sequence file: a path relative to the file holding this entry
    /// (`./sequences/calibrate.yaml`, `../shared/bench.yaml`), or
    /// `<package>:<path inside the package>` for a file shipped in a Python
    /// package of the procedure's environment. Never absolute, never `~`.
    #[serde(deserialize_with = "trimmed_text")]
    pub uses: String,
    /// Values for the sequence's `inputs:`, by input name. Every input without
    /// a default must be given; a name the sequence does not declare, or a
    /// value of the wrong type, refuses the procedure.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, serde_json::Value>,
    /// Bindings of the sequence's plug requirements to the procedure's plugs:
    /// the requirement's key to the procedure plug's key. A requirement whose
    /// key matches a procedure plug is bound without an entry here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugs: BTreeMap<String, String>,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
    /// false skips the whole call. Default true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Keys of phases or sequence calls of this file this call waits for.
    /// Ordering only, like a phase's depends_on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// On a call in the procedure's setup or teardown: slot (once per slot,
    /// the default) or execution (once for all slots), inherited by every
    /// phase of the sequence. Not allowed in main, where a call always runs
    /// per slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// What happens after the sequence, per outcome of the SEQUENCE (fail,
    /// error, timeout): continue or stop. `pass` and `retry` are refused on
    /// a call. Overrides execution.on_first_failure for this call. Inside the sequence, a failing phase always ends the
    /// sequence (its remaining phases skip, its teardown runs); this decides
    /// what the procedure does with that result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,
}

impl SequenceCallYaml {
    /// The instance key: `key:` when stated, else derived from `name:`,
    /// else from the sequence file's name (`fallback_name`).
    pub fn instance_key(&self, fallback_name: &str) -> String {
        if let Some(key) = &self.key {
            return key.clone();
        }
        let name = self.name.as_deref().unwrap_or(fallback_name);
        crate::python::to_python_identifier(name)
    }
}

/// The two shapes a setup, main or teardown entry can take: a phase, or a
/// sequence call. Deserialized through `RawEntryYaml` (every field of
/// both, one `deny_unknown_fields`) so a typo keeps its line number and
/// a field of the wrong shape is refused by name, which neither
/// `untagged` ("did not match any variant") nor a `Value` detour (no
/// position) can give.
#[derive(Debug, Clone)]
pub enum PhaseEntryYaml {
    /// Boxed: a phase is twice the size of a sequence call.
    Phase(Box<PhaseDefinitionYaml>),
    Uses(SequenceCallYaml),
}

impl PhaseEntryYaml {
    pub fn as_phase(&self) -> Option<&PhaseDefinitionYaml> {
        match self {
            PhaseEntryYaml::Phase(p) => Some(p),
            PhaseEntryYaml::Uses(_) => None,
        }
    }

    pub fn as_call(&self) -> Option<&SequenceCallYaml> {
        match self {
            PhaseEntryYaml::Uses(c) => Some(c),
            PhaseEntryYaml::Phase(_) => None,
        }
    }
}

/// Union of `PhaseDefinitionYaml` and `SequenceCallYaml` for
/// deserialization only. Field docs live on those two structs.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
struct RawEntryYaml {
    #[serde(default)]
    key: Option<String>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    name: Option<String>,
    #[serde(default)]
    scope: Option<Scope>,
    #[serde(default)]
    python: Option<PythonSpec>,
    #[serde(default)]
    executable: Option<ExecutableConfig>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    description: Option<String>,
    // The same refusal as the typed phase's field: an entry reaches
    // `PhaseDefinitionYaml` through this struct, so the check has to sit
    // here too or a limit on a series would load.
    #[serde(default, deserialize_with = "judgeable_measurements_opt")]
    measurements: Option<Vec<MeasurementSpecYaml>>,
    #[serde(default)]
    ui: Option<UIConfig>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_duration")]
    timeout: Option<u64>,
    #[serde(default)]
    retry: Option<RetryConfigYaml>,
    #[serde(default)]
    then: Option<ThenConfig>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    uses: Option<String>,
    #[serde(default)]
    with: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    plugs: Option<BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_optional_env")]
    env: Option<BTreeMap<String, String>>,
}

fn deserialize_optional_env<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_env(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for PhaseEntryYaml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        let raw = RawEntryYaml::deserialize(deserializer)?;
        match raw.uses {
            Some(uses) => {
                let label = raw
                    .name
                    .clone()
                    .or_else(|| raw.key.clone())
                    .unwrap_or_else(|| uses.clone());
                let mut phase_only: Vec<&str> = Vec::new();
                if raw.python.is_some() {
                    phase_only.push("python");
                }
                if raw.executable.is_some() {
                    phase_only.push("executable");
                }
                if raw.measurements.is_some() {
                    phase_only.push("measurements");
                }
                if raw.ui.is_some() {
                    phase_only.push("ui");
                }
                if raw.result.is_some() {
                    phase_only.push("result");
                }
                if raw.timeout.is_some() {
                    phase_only.push("timeout");
                }
                if raw.retry.is_some() {
                    phase_only.push("retry");
                }
                if raw.env.is_some() {
                    phase_only.push("env");
                }
                if !phase_only.is_empty() {
                    return Err(D::Error::custom(format!(
                        "sequence call `{}` (uses: {}) cannot take {}: these belong to a phase, not to a sequence call",
                        label,
                        uses,
                        phase_only
                            .iter()
                            .map(|f| format!("`{f}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                Ok(PhaseEntryYaml::Uses(SequenceCallYaml {
                    name: raw.name,
                    key: raw.key,
                    uses,
                    with: raw.with.unwrap_or_default(),
                    plugs: raw.plugs.unwrap_or_default(),
                    description: raw.description,
                    enabled: raw.enabled,
                    depends_on: raw.depends_on,
                    scope: raw.scope,
                    then: raw.then,
                }))
            }
            None => {
                if raw.plugs.is_some() {
                    return Err(D::Error::custom(
                        "`plugs` only belongs on a sequence call: add `uses: <sequence file>` to the entry, or remove it; a phase receives a plug by naming its key as a parameter",
                    ));
                }
                // A phase's `with:` is its function's keyword arguments: a
                // phase without `python:` has no function to receive them.
                if raw.with.is_some() && raw.python.is_none() {
                    return Err(D::Error::custom(
                        "`with` only belongs on a sequence call or on a phase with `python:` (the function's keyword arguments): add `uses: <sequence file>` or `python:` to the entry, or remove it",
                    ));
                }
                let name = raw.name.ok_or_else(|| D::Error::missing_field("name"))?;
                Ok(PhaseEntryYaml::Phase(Box::new(PhaseDefinitionYaml {
                    key: raw.key,
                    name,
                    scope: raw.scope,
                    python: raw.python,
                    executable: raw.executable,
                    description: raw.description,
                    measurements: raw.measurements.unwrap_or_default(),
                    ui: raw.ui,
                    enabled: raw.enabled,
                    result: raw.result,
                    depends_on: raw.depends_on,
                    timeout: raw.timeout,
                    retry: raw.retry,
                    then: raw.then,
                    with: raw.with.unwrap_or_default(),
                    env: raw.env.unwrap_or_default(),
                })))
            }
        }
    }
}

impl Serialize for PhaseEntryYaml {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            PhaseEntryYaml::Phase(p) => p.serialize(serializer),
            PhaseEntryYaml::Uses(c) => c.serialize(serializer),
        }
    }
}

/// `anyOf` of the two shapes, so the assistant reads a setup, main or
/// teardown entry as "a phase, or a sequence call".
#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for PhaseEntryYaml {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PhaseEntryYaml".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let phase = generator.subschema_for::<PhaseDefinitionYaml>();
        let call = generator.subschema_for::<SequenceCallYaml>();
        schemars::json_schema!({
            "description": "One entry of setup, main or teardown: a phase, or a sequence call (an entry with `uses:`).",
            "anyOf": [phase, call]
        })
    }
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PhaseDefinition {
    /// The phase's key as the engine addresses it: the author's key for a
    /// phase of the procedure, and `<call key>/<...>/<author's key>` for a
    /// phase a sequence call brought in, so two calls of one file never
    /// collide. Every `/`-separated segment is a Python identifier.
    #[validate(
        length(min = 1, max = 400),
        custom(function = "validate_phase_key_path")
    )]
    pub key: String,

    /// Instance keys of the sequence calls this phase came through, root
    /// first; empty for a phase written in the procedure itself. The
    /// scope the orchestrator cancels and settles by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequence_path: Vec<String>,

    /// Parallel to `sequence_path`: at each depth, the stage (setup, main,
    /// teardown) in which the next level, or at the last depth the phase
    /// itself, sits inside that sequence. A cancellation of a sequence
    /// keeps the jobs whose stage within it is teardown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequence_stages: Vec<PhaseStage>,

    /// The resolved `inputs:` of the innermost sequence this phase belongs
    /// to, handed to the phase function as the `inputs` injectable. Empty
    /// outside a sequence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, serde_json::Value>,

    /// Plug parameter name as the sequence's phases spell it to the
    /// procedure plug key it is bound to (`plugs:` on the call, or the
    /// same name). The worker injects the bound plug under the phase's
    /// own parameter name. Empty outside a sequence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plug_aliases: BTreeMap<String, String>,

    /// Where the phase was written when it came from a sequence file:
    /// the file and the entry's list and index in it, so a diagnostic
    /// points into that file. `None` for a phase of the procedure itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PhaseSource>,

    /// The entry as the sequence file writes it, for a phase a call
    /// brought in: the local key, the name and every field before the
    /// `{inputs.x}` substitution, `depends_on` as written (no barrier,
    /// no call dependency). What an editor of the file shows and writes
    /// back; the expanded phase around it is what runs. `None` for a
    /// phase of the procedure. Never serialized: the plan and the upload
    /// carry the expanded phase.
    #[serde(skip)]
    pub written: Option<Box<PhaseDefinition>>,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSpec>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableConfig>,

    #[validate(length(max = 1000))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<MeasurementSpec>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UIConfig>,

    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,

    #[validate(length(max = 50))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,

    #[validate(length(max = 100))]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    #[validate(range(min = 1, max = 86400000))]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub timeout: Option<u64>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,

    /// Keyword arguments of the phase function, markers in place
    /// (expanded per job at run start).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, serde_json::Value>,

    /// The phase's own `env:`, markers in place.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// The config and secrets this phase reads: those its sequence file
    /// declares, for a phase a call brought in. `None` for a phase of the
    /// procedure, which reads the procedure's own declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_scope: Option<super::values::ValueScope>,
}

impl From<PhaseDefinitionYaml> for PhaseDefinition {
    fn from(yaml: PhaseDefinitionYaml) -> Self {
        let key = yaml
            .key
            .unwrap_or_else(|| crate::python::to_python_identifier(&yaml.name));

        PhaseDefinition {
            key,
            sequence_path: Vec::new(),
            sequence_stages: Vec::new(),
            inputs: BTreeMap::new(),
            plug_aliases: BTreeMap::new(),
            source: None,
            written: None,
            name: yaml.name,
            scope: yaml.scope,
            python: yaml.python,
            executable: yaml.executable,
            description: yaml.description,
            measurements: yaml.measurements.into_iter().map(|m| m.into()).collect(),
            ui: yaml.ui,
            enabled: yaml.enabled,
            result: yaml.result,
            depends_on: yaml.depends_on,
            timeout: yaml.timeout,
            retry: yaml.retry.map(|r| r.into()),
            then: yaml.then,
            with: yaml.with,
            env: yaml.env,
            value_scope: None,
        }
    }
}

impl PhaseDefinition {
    /// The key the author wrote: the last segment of `key` for a phase
    /// that came through sequence calls, the whole key otherwise.
    pub fn local_key(&self) -> &str {
        self.key.rsplit('/').next().unwrap_or(&self.key)
    }

    /// The `sequence_path` joined by `/`, the value stored as the
    /// phase's `sequence_path` on the dashboard; `None` outside a sequence.
    pub fn sequence_path_str(&self) -> Option<String> {
        if self.sequence_path.is_empty() {
            None
        } else {
            Some(self.sequence_path.join("/"))
        }
    }

    /// Teardown work of some enclosing sequence: a cancellation of a
    /// sequence, a slot or the execution keeps such a job queued, the
    /// way it keeps the procedure's own teardown stages, so every
    /// sequence that was entered is left the way its author asked.
    pub fn is_sequence_teardown_work(&self) -> bool {
        self.sequence_stages.contains(&PhaseStage::Teardown)
    }

    pub fn to_yaml(&self) -> PhaseDefinitionYaml {
        // A phase brought in by a sequence call is written back through
        // the call, not as a phase: its key is then the local one.
        let auto_key = crate::python::to_python_identifier(&self.name);
        PhaseDefinitionYaml {
            key: if self.local_key() != auto_key {
                Some(self.local_key().to_string())
            } else {
                None
            },
            name: self.name.clone(),
            scope: self.scope,
            python: self.python.clone(),
            executable: self.executable.clone(),
            description: self.description.clone(),
            measurements: self.measurements.iter().map(|m| m.to_yaml()).collect(),
            ui: self.ui.clone(),
            enabled: self.enabled,
            result: self.result.clone(),
            depends_on: self.depends_on.clone(),
            timeout: self.timeout,
            retry: self.retry.as_ref().map(|r| r.into()),
            then: self.then.clone(),
            with: self.with.clone(),
            env: self.env.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for PhaseDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = PhaseDefinitionYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

impl PhaseDefinition {
    pub fn should_skip(&self) -> bool {
        !self.enabled || self.result.as_deref() == Some("skip")
    }

    pub fn to_python_identifier(&self) -> String {
        crate::python::to_python_identifier(&self.name)
    }

    pub fn get_key(&self) -> String {
        self.key.clone()
    }

    pub fn get_display_name(&self) -> String {
        self.name.clone()
    }

    pub fn validate_single_runtime(&self) -> Result<(), String> {
        let has_python = self.python.is_some();
        let has_executable = self.executable.is_some();

        if has_python && has_executable {
            return Err("set `python` or `executable`, not both".to_string());
        }

        Ok(())
    }
}

impl ProcedureDefinition {
    /// Ensure key is unique by appending counter if needed
    fn ensure_unique_key(key: String, seen_keys: &mut HashSet<String>) -> String {
        if !seen_keys.contains(&key) {
            seen_keys.insert(key.clone());
            return key;
        }

        let mut counter = 1;
        loop {
            let candidate = format!("{}-{}", key, counter);
            if !seen_keys.contains(&candidate) {
                seen_keys.insert(candidate.clone());
                return candidate;
            }
            counter += 1;
        }
    }

    /// What the lint says about the sequence calls, beyond what expansion
    /// refused: a sequence file that will not travel with a deployment,
    /// and a plug bound to a class other than the one the file names.
    ///
    /// The deployable tree is what a build tars: the enclosing uv
    /// workspace root when one exists (`clone-context.ts`), else the
    /// procedure directory alone. A file outside it loads here and is
    /// missing on the station, so it is an error, with the fix in the
    /// message. Only the calls written in the procedure (depth 1) get a
    /// location; a nested call's file is reported through its root call.
    pub fn sequence_problems(&self, procedure_dir: &Path) -> Vec<RefProblem> {
        let mut problems = Vec::new();
        if self.sequences.is_empty() {
            return problems;
        }
        let root = deployable_root(procedure_dir);
        let canonical_root = root.canonicalize().unwrap_or(root.clone());
        for inst in &self.sequences {
            let Some(top) = self
                .sequences
                .iter()
                .find(|s| s.path.len() == 1 && s.path[0] == inst.path[0])
            else {
                continue;
            };
            let at = RefLocation::key(
                match top.stage {
                    PhaseStage::Setup => RefList::Setup,
                    PhaseStage::Main => RefList::Main,
                    PhaseStage::Teardown => RefList::Teardown,
                },
                top.entry_index,
                "uses",
            );
            let file = Path::new(&inst.file);
            let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
            // A package file lives in the environment, which the build
            // installs on the station: nothing to ship. The same holds for
            // a file a package's sequence calls by a relative path: it is
            // shipped inside the package too.
            let from_package = self.sequences.iter().any(|ancestor| {
                inst.path.starts_with(&ancestor.path)
                    && super::expand::split_package_reference(&ancestor.source).is_some()
            });
            if !from_package && !canonical.starts_with(&canonical_root) {
                let where_ = if root == procedure_dir {
                    "the procedure directory".to_string()
                } else {
                    format!("the uv workspace at {}", root.display())
                };
                problems.push(RefProblem::error(
                    codes::SEQUENCE_OUTSIDE_ROOT,
                    format!(
                        "Sequence `{}`: {} is outside {where_}, so it will not be shipped with the deployment. Move it under that directory, declare a uv workspace at a common ancestor, or deliver it in a Python package (`<package>:<path>`)",
                        inst.path_str(),
                        inst.file
                    ),
                    at.clone(),
                ));
            }
            // The class the file was written against versus the class the
            // bound procedure plug instantiates: a warning, since a
            // compatible driver under another name is a legitimate binding.
            for (requirement, bound) in &inst.plugs {
                let Some(plug) = self.plugs.iter().find(|p| &p.key == bound) else {
                    continue;
                };
                let Some(expected) = self.sequence_requirement_class(inst, requirement) else {
                    continue;
                };
                let actual = plug.python.get_callable_name();
                if expected != actual {
                    problems.push(RefProblem {
                        code: codes::SEQUENCE_PLUG_CLASS_MISMATCH,
                        severity: RefSeverity::Warning,
                        message: format!(
                            "Sequence `{}`: plug `{requirement}` was written against class `{expected}`, and is bound to `{bound}`, a `{actual}`. Fine when the two drivers answer the same calls",
                            inst.path_str()
                        ),
                        location: at.clone(),
                        hint: None,
                    });
                }
            }
        }
        problems
    }

    /// The class name a sequence file's plug requirement states, read
    /// from the file (the expansion keeps bindings, not requirements).
    fn sequence_requirement_class(
        &self,
        inst: &SequenceInstance,
        requirement: &str,
    ) -> Option<String> {
        inst.declared_plugs()
            .iter()
            .find(|p| p.key() == requirement)
            .and_then(|p| p.python.as_ref())
            .map(|spec| spec.get_callable_name())
    }

    /// Iterator over all phases with their stage scope (standardized iteration)
    pub fn iter_phases_with_stage(&self) -> impl Iterator<Item = (StageScope, &PhaseDefinition)> {
        self.setup
            .iter()
            .map(|p| {
                let scope = if p.scope == Some(Scope::Execution) {
                    StageScope::SetupAll
                } else {
                    StageScope::SetupEach
                };
                (scope, p)
            })
            .chain(self.main.iter().map(|p| (StageScope::Main, p)))
            .chain(self.teardown.iter().map(|p| {
                let scope = if p.scope == Some(Scope::Execution) {
                    StageScope::TeardownAll
                } else {
                    StageScope::TeardownEach
                };
                (scope, p)
            }))
    }

    /// Phase keys whose results the phase `phase_key` (running in `stage`)
    /// may read: its own (previous attempt), every phase of an earlier
    /// stage, and the transitive closure of its `depends_on`. Anything
    /// else is only complete by luck of scheduling. Shared by the
    /// orchestrator (which withholds the rest) and the lint (which warns
    /// about a parameter naming one).
    ///
    /// A shared phase (one job for the execution: `scope: execution`
    /// setup or teardown) only ever receives shared results: there is one
    /// result per slot for every other phase, and none to pick.
    pub fn visible_phase_keys(&self, phase_key: &str, stage: StageScope) -> HashSet<String> {
        let all = self.get_all_phases_with_stage_scope();
        let stage_of: HashMap<&str, StageScope> =
            all.iter().map(|(s, p)| (p.key.as_str(), *s)).collect();
        let reachable = |key: &str| -> bool {
            !stage.is_shared() || stage_of.get(key).is_some_and(|s| s.is_shared())
        };
        let mut visible = HashSet::new();
        visible.insert(phase_key.to_string());
        for (other_stage, phase) in &all {
            if other_stage.rank() < stage.rank() && reachable(&phase.key) {
                visible.insert(phase.key.clone());
            }
        }
        let by_key: HashMap<&str, &PhaseDefinition> =
            all.iter().map(|(_, p)| (p.key.as_str(), *p)).collect();
        let mut stack = vec![phase_key.to_string()];
        while let Some(key) = stack.pop() {
            if let Some(phase) = by_key.get(key.as_str()) {
                for dep in &phase.depends_on {
                    if reachable(dep) && visible.insert(dep.clone()) {
                        stack.push(dep.clone());
                    }
                }
            }
        }
        visible
    }

    /// Get all phases with their stages for execution (calls standardized iterator)
    pub fn get_all_phases_with_stage_scope(&self) -> Vec<(StageScope, &PhaseDefinition)> {
        self.iter_phases_with_stage().collect()
    }

    /// The procedure lint: everything that must hold before a run and
    /// that LOADING lets through, one finding per problem with the entry
    /// it points at. Every `python:` reference resolved against the
    /// procedure directory, the referenced symbol present in its file, a
    /// plug's `config:` keys matching its class's `__init__`, and an
    /// executable phase whose command is still empty.
    ///
    /// Loading is purely structural (no disk access), so all of these
    /// pass `load_procedure_definition` and would otherwise surface
    /// mid-run: a dangling phase ref as a tp_worker traceback, a dangling
    /// plug ref as a silently omitted plug (`get_plug_configs_for_job`
    /// only logs), a typo in a class name or a config key as a
    /// `TypeError` at setup — on a bench, with hardware committed — and
    /// worse for an empty command, since `sh -c ""` exits 0, so it is a
    /// phase that silently passes without doing anything. Callers decide
    /// the consequence: the studio daemon turns these into diagnostics,
    /// the run engine refuses to start on the `Error` ones.
    ///
    /// The symbol and config rules read the `.py` file as TEXT, never
    /// importing it (`pysource`) — the same column-0 scan the Studio
    /// Inspector already applies, so both surfaces agree, and safe to run
    /// on every save and every keystroke. A signature read off annotated
    /// attributes of a plain class (no `@dataclass`, no `BaseModel`) may
    /// belong to a base in another file, so mismatches against it are
    /// `Warning`s; a `def __init__` or a generated constructor is certain.
    ///
    /// Mirrors what actually happens at run time, no stricter:
    /// - Plugs go through `parse` at job build (`to_config_json`), so on a
    ///   full run every declared plug file is genuinely required on disk.
    ///   On a partial run (`main_filter` given) plugs are NOT gated at all:
    ///   the runtime narrows the plug set via async Python signature
    ///   introspection (`union_required_plugs`), which a synchronous gate
    ///   cannot reproduce — gating them all would refuse runs the runtime
    ///   would start (the mid-authoring "play one phase" loop in Studio).
    ///   `validate` still reports every missing plug file, and a plug the
    ///   partial run actually needs still fails at job build as before.
    /// - Phases are loaded by tp_worker, which falls back to importlib
    ///   for bare dot syntax (workspace wheels) — a missing file is only
    ///   an error when the spec is tree-bound (`is_tree_bound`).
    /// - Phases the orchestrator never turns into a job don't gate:
    ///   `should_skip()` ones, and main phases outside `main_filter`
    ///   (a partial run's dependency closure) when one is given.
    pub fn resolve_runtime_refs(
        &self,
        procedure_dir: &Path,
        main_filter: Option<&HashSet<String>>,
    ) -> Vec<RefProblem> {
        use super::pysource;

        // The dotted-class spelling (`plugs.psu.PowerSupply` for class
        // PowerSupply in plugs/psu.py) is the most common way to land
        // here: every dot is a directory, the class needs a ':'. When
        // swapping the last '.' for ':' makes the spec parse, say so —
        // "file not found: .../PowerSupply.py" alone reads as a broken
        // validator to whoever meant the class.
        let modules: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
        let colon_hint = |spec: &PythonSpec| -> String {
            let raw = spec.as_str().trim();
            // `bad.py:Bad` for bad.py at the root: a dotted module path
            // reads it as bad/py.py. Both `bad:Bad` and `./bad.py:Bad` work.
            if let Some((module, name)) = raw.split_once(':') {
                if let Some(stem) = module.strip_suffix(".py") {
                    if !stem.is_empty() && !module.contains('/') && !module.contains('\\') {
                        let alt = format!("{}:{name}", stem);
                        if PythonSpec(alt.clone()).parse(procedure_dir).is_ok() {
                            return format!("; did you mean `{alt}`?");
                        }
                    }
                }
            }
            // A misspelt dotted module (`plug.psu:Psu` for plugs/psu.py):
            // the nearest module of the project.
            if let Some((module, name)) = raw.split_once(':') {
                if !module.contains('/') && !module.contains('\\') {
                    let modules = modules.get_or_init(|| project_modules(procedure_dir));
                    if let Some(near) =
                        pysource::closest(module, modules.iter().map(String::as_str))
                    {
                        return format!("; did you mean `{near}:{name}`?");
                    }
                }
                return String::new();
            }
            if raw.contains('/') || raw.contains('\\') {
                return String::new();
            }
            let Some((head, tail)) = raw.rsplit_once('.') else {
                return String::new();
            };
            let alt = format!("{head}:{tail}");
            if PythonSpec(alt.clone()).parse(procedure_dir).is_ok() {
                format!("; did you mean `{alt}`?")
            } else {
                String::new()
            }
        };
        let mut problems = Vec::new();
        problems.extend(self.sequence_problems(procedure_dir));
        // `scope:` only places a setup or teardown phase: a main phase
        // runs once per slot whatever it says.
        for (index, phase) in self.main.iter().enumerate() {
            if phase.scope == Some(Scope::Execution) && phase.sequence_path.is_empty() {
                problems.push(RefProblem {
                    code: codes::PHASE_SCOPE_IGNORED,
                    severity: RefSeverity::Warning,
                    message: format!(
                        "Phase `{}`: `scope: execution` has no effect on a main phase, which runs once per slot; move it to `setup:` or `teardown:` to run it once",
                        phase.key
                    ),
                    location: self.written_location(RefList::Main, index, "scope"),
                    hint: None,
                });
            }
        }
        // `auto_identify` on several slots with one serial for all: every
        // slot records the same unit, as `--ui-values` refuses it.
        let slots = self.execution.as_ref().map_or(0, |e| e.slots.len());
        if let Some(unit) = self.unit.as_ref().filter(|u| u.auto_identify && slots > 1) {
            let serial = unit
                .serial_number
                .as_ref()
                .and_then(|f| f.default_value.as_deref());
            if let Some(serial) = serial.filter(|s| {
                !super::markers::find_markers(s)
                    .iter()
                    .any(|m| m.reference.reads_slot())
            }) {
                problems.push(RefProblem {
                    code: codes::UNIT_SERIAL_SHARED,
                    severity: RefSeverity::Warning,
                    message: format!(
                        "every one of the {slots} slots identifies as `{serial}`; put `{{run.slot.key}}` in it (`{serial}-{{run.slot.key}}`) so each slot records its own unit"
                    ),
                    location: RefLocation::key(RefList::Root, 0, "unit.serial_number.default_value"),
                    hint: None,
                });
            }
        }
        if main_filter.is_none() {
            for (index, plug) in self.plugs.iter().enumerate() {
                let at = |key: &str| RefLocation::key(RefList::Plugs, index, key);
                let (file, class) = match plug.python.parse(procedure_dir) {
                    // An installed package's class (a vendor driver), which
                    // the venv shows: imported by module name at run time,
                    // checked from its source when that is plain Python.
                    Err(_)
                        if plug.python.installed_module(procedure_dir).is_some()
                            && installed_in_venv(procedure_dir, &plug.python.get_module()) =>
                    {
                        match (
                            venv_module_file(procedure_dir, &plug.python.get_module()),
                            plug.python.installed_module(procedure_dir),
                        ) {
                            // A module-level `__getattr__` builds its
                            // names at import (lazy exports): what it
                            // offers cannot be read from the text.
                            (Some(file), Some((_, class)))
                                if !std::fs::read_to_string(&file).is_ok_and(|t| {
                                    t.lines().any(|l| l.starts_with("def __getattr__("))
                                }) =>
                            {
                                (file, class)
                            }
                            _ => continue,
                        }
                    }
                    // Declared as a dependency, but the venv does not have
                    // it yet (a fresh checkout, a CI job before its first
                    // run, a dependency added since the venv was built):
                    // the run syncs the venv first, so it is not checked
                    // rather than refused.
                    Err(_)
                        if plug.python.installed_module(procedure_dir).is_some()
                            && declares_dependency(procedure_dir, &plug.python.get_module()) =>
                    {
                        problems.push(RefProblem {
                            code: codes::NOT_CHECKED,
                            severity: RefSeverity::Warning,
                            message: format!(
                                "Plug `{}`: `{}` is a declared dependency the venv does not have yet; checked once a run has synced it",
                                plug.key,
                                plug.python.get_module()
                            ),
                            location: at("python"),
                            hint: None,
                        });
                        continue;
                    }
                    Err(e) => {
                        let package = if plug.python.installed_module(procedure_dir).is_some() {
                            format!(
                                "; for an installed package, add `{}` to the dependencies",
                                plug.python
                                    .get_module()
                                    .split('.')
                                    .next()
                                    .unwrap_or_default()
                            )
                        } else {
                            String::new()
                        };
                        problems.push(RefProblem::error(
                            codes::PLUG_PYTHON_INVALID,
                            format!(
                                "Plug `{}`: {}{}{package}",
                                plug.key,
                                e,
                                colon_hint(&plug.python)
                            ),
                            at("python"),
                        ));
                        continue;
                    }
                    Ok(resolved) => resolved,
                };
                // Unreadable (permissions, binary): cannot tell, say nothing.
                let Ok(source) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let classes = pysource::top_level_classes(&source);
                // Seen by the line scan only: a syntax error above it (the
                // syntax check reports that one).
                if !pysource::binds_top_level(&source, &class) && !classes.contains(&class) {
                    let hint = pysource::closest(&class, classes.iter().map(String::as_str))
                        .map(|near| {
                            let spec = plug.python.as_str();
                            let module = spec.rsplit_once(':').map_or(spec, |(m, _)| m);
                            format!("; did you mean `{module}:{near}`?")
                        })
                        .unwrap_or_default();
                    problems.push(RefProblem::error(
                        codes::PLUG_CLASS_NOT_FOUND,
                        format!(
                            "Plug `{}`: class `{}` not found in {}{hint}",
                            plug.key,
                            class,
                            relative_to(&file, procedure_dir)
                        ),
                        at("python"),
                    ));
                    continue;
                }
                // `with:` is splatted into the class as kwargs
                // (`plug_class(**config)` in tp_plug.py), so a key the constructor does
                // not take, or a required one it does not get, is a
                // TypeError at setup. `None`: the signature is not
                // knowable from this file — issue nothing.
                let Some(sig) = class_init_signature(&file, source.clone(), &class, procedure_dir)
                else {
                    continue;
                };
                let severity = if sig.certain {
                    RefSeverity::Error
                } else {
                    RefSeverity::Warning
                };
                // The key as the file spells it, for the messages.
                let with = plug.with_key();
                let keys: Vec<&String> = plug
                    .with
                    .as_ref()
                    .map(|m| m.keys().collect())
                    .unwrap_or_default();
                if !sig.kwargs {
                    for key in &keys {
                        if sig.params.iter().any(|p| &p.name == *key) {
                            continue;
                        }
                        let hint = pysource::suggestion_for(key, &sig.params)
                            .map(|s| format!("; did you mean `{s}`?"))
                            .unwrap_or_default();
                        problems.push(RefProblem {
                            code: codes::PLUG_UNKNOWN_PARAM,
                            severity,
                            message: format!(
                                "Plug `{}`: `{}` is not a parameter of `{}.__init__`{}",
                                plug.key, key, class, hint
                            ),
                            location: at(&format!("{with}.{key}")),
                            hint: None,
                        });
                    }
                }
                for param in sig.params.iter().filter(|p| p.required) {
                    // The framework injects `dashboard` into a plug's
                    // `__init__`; `with:` may not set it (refused at load).
                    if param.name == "dashboard" || keys.iter().any(|k| **k == param.name) {
                        continue;
                    }
                    problems.push(RefProblem {
                        code: codes::PLUG_MISSING_PARAM,
                        severity,
                        message: format!(
                            "Plug `{}`: missing `{with}.{}`, required by `{}.__init__`",
                            plug.key, param.name, class
                        ),
                        location: if plug.with.is_some() {
                            at(with)
                        } else {
                            RefLocation::entry(RefList::Plugs, index)
                        },
                        hint: None,
                    });
                }
            }
        }
        // Parameters the worker resolves before looking at phase results.
        const WORKER_BUILTINS: &[&str] = super::value_checks::INJECTABLES;
        let stage_of: HashMap<&str, StageScope> = self
            .get_all_phases_with_stage_scope()
            .into_iter()
            .map(|(stage, p)| (p.key.as_str(), stage))
            .collect();
        let phase_keys: HashSet<&str> = stage_of.keys().copied().collect();

        // Panels are shipped to every viewer and bound by name: a
        // forbidden element or a binding to nothing is refused here, before
        // hardware is committed. Both kinds read every phase's measurements
        // by phase key, and the plugs.
        let panel_names =
            |procedure, components, measurements, source| crate::ui::panel::names::Names {
                procedure,
                components,
                measurements,
                phases: self
                    .get_all_phases_with_stage_scope()
                    .into_iter()
                    .map(|(_, p)| {
                        (
                            p.key.as_str(),
                            p.measurements.iter().map(|m| m.key.as_str()).collect(),
                        )
                    })
                    .collect(),
                plugs: self.plugs.iter().map(|p| p.key.as_str()).collect(),
                source,
            };
        let panel_problems = |rel: &str, names, prefix: &str, location: RefLocation| {
            crate::ui::panel::lint(procedure_dir, rel, &names)
                .into_iter()
                .map(move |finding| RefProblem {
                    code: codes::UI_PANEL,
                    severity: if finding.error {
                        RefSeverity::Error
                    } else {
                        RefSeverity::Warning
                    },
                    message: format!("{prefix}{}", finding.message),
                    location: location.clone(),
                    hint: None,
                })
                .collect::<Vec<_>>()
        };
        // The run panel: no components, `measurements` is the phase
        // running at the time, so any phase's.
        if let Some(rel) = self.ui.as_ref().and_then(|u| u.panel.as_deref()) {
            let measurements = self
                .get_all_phases_with_stage_scope()
                .into_iter()
                .flat_map(|(_, p)| p.measurements.iter().map(|m| m.key.as_str()))
                .collect();
            problems.extend(panel_problems(
                rel,
                panel_names(true, &[], measurements, None),
                "",
                RefLocation::key(RefList::Root, 0, "ui.panel"),
            ));
        }
        let lists = [
            (RefList::Setup, &self.setup),
            (RefList::Main, &self.main),
            (RefList::Teardown, &self.teardown),
        ];
        for (list, phases) in lists {
            for (index, phase) in phases.iter().enumerate() {
                if phase.should_skip() {
                    continue;
                }
                if let Some(filter) = main_filter {
                    if list == RefList::Main && !filter.contains(&phase.key) {
                        continue;
                    }
                }
                let at = |key: &str| self.written_location(list, index, key);
                // Two specs under one key: the worker records both under
                // the same name, so validators and aggregations read
                // whichever landed last — wrong data, silently. A v1 rule.
                let mut seen = HashSet::new();
                for m in &phase.measurements {
                    if !seen.insert(m.key.as_str()) {
                        problems.push(RefProblem::error(
                            codes::DUPLICATE_MEASUREMENT_KEY,
                            format!("Phase `{}`: duplicate measurement key {}", phase.key, m.key),
                            at("measurements"),
                        ));
                    }
                }
                // A limit on an axis is judged against the whole series,
                // which only `==` and `!=` compare: any other operator is
                // left UNSET at run time. Same sentence the evaluator logs,
                // so the two never drift. A warning: the run still starts.
                for m in &phase.measurements {
                    let axes = m.x_axis.iter().chain(m.y_axis.iter().flatten());
                    for axis in axes {
                        for v in axis.validators.iter().flatten() {
                            let Some(operator) = v.operator else { continue };
                            if matches!(operator, ValidatorOperator::Eq | ValidatorOperator::Ne) {
                                continue;
                            }
                            let issue = MatchIssue::UnsupportedAxisOperator {
                                measurement: m.key.clone(),
                                axis: axis.get_key(),
                                operator: operator.as_str().to_string(),
                            };
                            problems.push(RefProblem {
                                code: codes::MEASUREMENT_AXIS_OPERATOR,
                                severity: RefSeverity::Warning,
                                message: format!("Phase `{}`: {issue}", phase.key),
                                location: at("measurements"),
                                hint: None,
                            });
                        }
                    }
                }
                // A lower limit above the upper one: the measurement can
                // never pass, whatever the unit measures. A warning: the
                // run still starts, and still records the values.
                for m in &phase.measurements {
                    // (limit, strict) of the tightest bound on each side.
                    let mut low: Option<(f64, bool)> = None;
                    let mut high: Option<(f64, bool)> = None;
                    for v in m.validators.iter().flatten() {
                        let (Some(operator), Some(ValidatorExpectedValue::Number(n))) =
                            (v.operator, v.expected_value.as_ref())
                        else {
                            continue;
                        };
                        let n = *n;
                        match operator {
                            ValidatorOperator::Ge | ValidatorOperator::Gt => {
                                let strict = operator == ValidatorOperator::Gt;
                                if low.is_none_or(|(l, s)| n > l || (n == l && strict && !s)) {
                                    low = Some((n, strict));
                                }
                            }
                            ValidatorOperator::Le | ValidatorOperator::Lt => {
                                let strict = operator == ValidatorOperator::Lt;
                                if high.is_none_or(|(h, s)| n < h || (n == h && strict && !s)) {
                                    high = Some((n, strict));
                                }
                            }
                            _ => {}
                        }
                    }
                    if let (Some((l, ls)), Some((h, hs))) = (low, high) {
                        if l > h || (l == h && (ls || hs)) {
                            let op = |strict: bool, lower: bool| match (lower, strict) {
                                (true, true) => ">",
                                (true, false) => ">=",
                                (false, true) => "<",
                                (false, false) => "<=",
                            };
                            problems.push(RefProblem {
                                code: codes::MEASUREMENT_LIMITS_CONTRADICT,
                                severity: RefSeverity::Warning,
                                message: format!(
                                    "Phase `{}`: measurement `{}` can never pass: `{} {l}` and `{} {h}` leave no value between them",
                                    phase.key,
                                    m.key,
                                    op(ls, true),
                                    op(hs, false)
                                ),
                                location: at("measurements"),
                                hint: None,
                            });
                        }
                    }
                }
                // A bind to a measurement the phase does not declare
                // records a measurement of its own, and leaves the
                // declared one unset: one edit from a declared key, that
                // is the mistake (an error); otherwise the undeclared one
                // may be meant (a warning, with a suggestion when close).
                for (ci, c) in phase
                    .ui
                    .iter()
                    .flat_map(|u| u.components.iter().flatten())
                    .enumerate()
                {
                    let Some(bound) = c.bind.as_deref().and_then(|b| {
                        b.strip_prefix("measurements.")
                            .or_else(|| b.strip_prefix("measurement."))
                    }) else {
                        continue;
                    };
                    if bound.is_empty()
                        || phase.measurements.is_empty()
                        || phase.measurements.iter().any(|m| m.key == bound)
                    {
                        continue;
                    }
                    let near =
                        pysource::closest(bound, phase.measurements.iter().map(|m| m.key.as_str()));
                    // Short keys (`v1`, `v2`, `v3`) are one edit apart by
                    // design: no proof of a typo there.
                    let typo = bound.chars().count() > 3
                        && near.is_some_and(|n| {
                            pysource::edit_distance(&n.to_lowercase(), &bound.to_lowercase())
                                == Some(1)
                        });
                    problems.push(RefProblem {
                        code: codes::UI_BIND_UNDECLARED,
                        severity: if typo {
                            RefSeverity::Error
                        } else {
                            RefSeverity::Warning
                        },
                        message: format!(
                            "Phase `{}`: `{}` binds to measurements.{bound}, which the phase does not declare{}",
                            phase.key,
                            c.key,
                            near.map(|n| format!("; did you mean `measurements.{n}`?"))
                                .unwrap_or_default()
                        ),
                        location: at(&format!("ui.components[{ci}].bind")),
                        hint: None,
                    });
                }
                // A text component bound to a measurement records the
                // operator's answer as text (a switch as true/false), and
                // a numeric limit never passes text: it reads FAIL on
                // every run. A warning: phase code may record the
                // measurement over the bound answer.
                for c in phase.ui.iter().flat_map(|u| u.components.iter().flatten()) {
                    if !c.component_type.is_input()
                        || matches!(
                            c.component_type,
                            UIComponentType::NumberInput | UIComponentType::Slider
                        )
                    {
                        continue;
                    }
                    let Some(bound) = c.bind.as_deref().and_then(|b| {
                        b.strip_prefix("measurements.")
                            .or_else(|| b.strip_prefix("measurement."))
                    }) else {
                        continue;
                    };
                    let Some(m) = phase.measurements.iter().find(|m| m.key == bound) else {
                        continue;
                    };
                    let numeric = m.validators.iter().flatten().find_map(|v| {
                        v.operator.filter(|o| {
                            matches!(
                                o,
                                ValidatorOperator::Lt
                                    | ValidatorOperator::Le
                                    | ValidatorOperator::Gt
                                    | ValidatorOperator::Ge
                            )
                        })
                    });
                    let Some(operator) = numeric else { continue };
                    let records = match c.component_type {
                        UIComponentType::Switch => "true or false",
                        _ => "text",
                    };
                    problems.push(RefProblem {
                        code: codes::UI_MEASUREMENT_TYPE_MISMATCH,
                        severity: RefSeverity::Warning,
                        message: format!(
                            "Phase `{}`: `{}` (`{}`) records {records} into measurement {}, whose `{}` limit compares numbers: reads FAIL on every run; use `number_input` or `slider`",
                            phase.key,
                            c.key,
                            c.component_type.as_str(),
                            m.key,
                            operator.as_str()
                        ),
                        location: at("ui"),
                        hint: None,
                    });
                }
                // Components sharing a key share one value slot: the last
                // one's default seeds it, all of them read it back, and
                // each bound one records it under its own measurement. A
                // warning and not an error, unlike measurements above:
                // nothing ever checked this, keys are mostly derived from
                // labels, and a procedure running in the field today has
                // to keep loading.
                let mut counts: Vec<(&str, usize)> = Vec::new();
                for c in phase.ui.iter().flat_map(|u| u.components.iter().flatten()) {
                    match counts.iter_mut().find(|(key, _)| *key == c.key) {
                        Some((_, n)) => *n += 1,
                        None => counts.push((c.key.as_str(), 1)),
                    }
                }
                for (key, n) in counts.into_iter().filter(|(_, n)| *n > 1) {
                    problems.push(RefProblem {
                        code: codes::UI_DUPLICATE_KEY,
                        severity: RefSeverity::Warning,
                        message: format!(
                            "Phase `{}`: key `{}` used by {} operator UI components, which share one value; give each its own `key:`",
                            phase.key, key, n
                        ),
                        location: at("ui"),
                        hint: None,
                    });
                }
                if let Some((ui, rel)) = phase
                    .ui
                    .as_ref()
                    .and_then(|u| Some((u, u.panel.as_deref()?)))
                {
                    let names = panel_names(
                        false,
                        ui.components.as_deref().unwrap_or(&[]),
                        phase.measurements.iter().map(|m| m.key.as_str()).collect(),
                        phase
                            .python
                            .as_ref()
                            .and_then(|p| p.resolve_path(procedure_dir).ok())
                            .and_then(|(file, _)| std::fs::read_to_string(file).ok()),
                    );
                    problems.extend(panel_problems(
                        rel,
                        names,
                        &format!("Phase `{}`: ", phase.key),
                        at("ui.panel"),
                    ));
                }
                // One finding per component, naming every idle field: five
                // pasted styling lines are one mistake, not five.
                for c in phase.ui.iter().flat_map(|u| u.components.iter().flatten()) {
                    let inert = c.inert_fields();
                    if inert.is_empty() {
                        continue;
                    }
                    let (names, verb, them) = match inert.as_slice() {
                        [one] => (format!("`{one}`"), "has", "it"),
                        many => (
                            many.iter()
                                .map(|n| format!("`{n}`"))
                                .collect::<Vec<_>>()
                                .join(", "),
                            "have",
                            "them",
                        ),
                    };
                    problems.push(RefProblem {
                        code: codes::UI_INEFFECTIVE_PROPERTY,
                        severity: RefSeverity::Warning,
                        message: format!(
                            "Phase `{}`: {} {} no effect on `{}` (`{}`); remove {them} or change the component type",
                            phase.key,
                            names,
                            verb,
                            c.key,
                            c.component_type.as_str()
                        ),
                        location: at("ui"),
                        hint: None,
                    });
                }
                if let Some(exec) = &phase.executable {
                    if exec.command.trim().is_empty() {
                        problems.push(RefProblem::error(
                            codes::EXECUTABLE_COMMAND_EMPTY,
                            format!("Phase `{}`: `executable.command` empty", phase.key),
                            at("executable.command"),
                        ));
                    }
                }
                // A choice component declared without options is filled at
                // run time by the phase (`ui.set(key, options=[...])`) or
                // never: the operator then sees "Waiting for options" and
                // cannot answer. Named here when nothing can fill it, and
                // below when the phase's Python does not.
                let unfilled: Vec<(&str, bool)> = phase
                    .ui
                    .iter()
                    .flat_map(|u| u.components.iter().flatten())
                    .filter(|c| {
                        matches!(
                            c.component_type,
                            UIComponentType::Radio
                                | UIComponentType::Select
                                | UIComponentType::Multiselect
                                | UIComponentType::Checklist
                        ) && c.options.as_ref().is_none_or(|o| o.is_empty())
                    })
                    .map(|c| (c.key.as_str(), c.required))
                    .collect();
                let unfilled_warning = |key: &str, how: &str| {
                    RefProblem {
                    code: codes::UI_OPTIONS_UNFILLED,
                    severity: RefSeverity::Warning,
                    message: format!(
                        "Phase `{}`: `{}` has no options and {}; add `options:` or call `ui.set(\"{}\", options=[...])` in the phase",
                        phase.key, key, how, key
                    ),
                    location: at("ui"),
                    hint: None,
                }
                };
                if phase.python.is_none() {
                    // Nothing can ever fill it. Required: a prompt nobody can
                    // answer, an error `run` stops on (Studio keeps loading
                    // the file and shows it). Optional: submits empty, a
                    // warning.
                    for (key, required) in &unfilled {
                        let mut problem = unfilled_warning(key, "no Python phase can fill them");
                        if *required {
                            problem.severity = RefSeverity::Error;
                        }
                        problems.push(problem);
                    }
                }
                let Some(spec) = &phase.python else { continue };
                match spec.resolve_path(procedure_dir) {
                    Err(e) => problems.push(RefProblem::error(
                        codes::PHASE_PYTHON_INVALID,
                        format!("Phase `{}`: {}", phase.key, e),
                        at("python"),
                    )),
                    Ok((file, callable)) => {
                        if !crate::python::is_valid_python_identifier(&callable) {
                            problems.push(RefProblem::error(
                                codes::PHASE_PYTHON_INVALID,
                                format!(
                                    "Phase `{}`: invalid `{}`: expected a Python identifier",
                                    phase.key, callable
                                ),
                                at("python"),
                            ));
                        } else if !file.exists() {
                            if spec.is_tree_bound(procedure_dir) {
                                problems.push(RefProblem::error(
                                    codes::PHASE_FILE_NOT_FOUND,
                                    format!(
                                        "Phase `{}`: Python file not found: {}{}{}",
                                        phase.key,
                                        relative_to(&file, procedure_dir),
                                        colon_hint(spec),
                                        self.sequences
                                            .iter()
                                            .find(|s| s.path == phase.sequence_path)
                                            .and_then(|s| Path::new(&s.file).parent())
                                            .map(|dir| {
                                                sequence_parent_hint(
                                                    &file,
                                                    dir,
                                                    procedure_dir,
                                                    &callable,
                                                )
                                            })
                                            .unwrap_or_default()
                                    ),
                                    at("python"),
                                ));
                            } else if !installed_in_venv(procedure_dir, &spec.get_module()) {
                                // Not in the tree: tp_worker falls back to
                                // importlib, so this may be a package in the
                                // environment — or a typo the runtime will
                                // catch. Say so, without blocking.
                                problems.push(RefProblem {
                                    code: codes::PHASE_MODULE_NOT_IN_PROJECT,
                                    severity: RefSeverity::Warning,
                                    message: format!(
                                        "Phase `{}`: Python file not found in the project: {}; `{}` will be imported from the venv at run time, check the spelling{}",
                                        phase.key,
                                        relative_to(&file, procedure_dir),
                                        spec.get_module(),
                                        colon_hint(spec)
                                    ),
                                    location: at("python"),
                                    hint: None,
                                });
                            }
                        } else if let Ok(written) = std::fs::read_to_string(&file) {
                            // Where the function is defined: a package
                            // `__init__` re-exporting it, or an alias.
                            let (_, source, callable) =
                                follow_import(&file, written, &callable, procedure_dir);
                            // `with:` is the function's keyword arguments:
                            // a key it does not take, or a parameter
                            // without a default that nothing fills, is a
                            // TypeError when the phase starts. `None`: the
                            // function is not a top-level def of this file
                            // (reported below), nothing to compare.
                            // A `vars` parameter is checked without `with:`
                            // too: the old name of `config` fails every run.
                            if let Some(params) = pysource::function_signature(&source, &callable) {
                                let named = |name: &str| {
                                    params.iter().any(|p| p.stars == 0 && p.name == name)
                                };
                                if !phase.with.is_empty() && !params.iter().any(|p| p.stars == 2) {
                                    for key in phase.with.keys().filter(|k| !named(k)) {
                                        problems.push(RefProblem::error(
                                            codes::PHASE_UNKNOWN_PARAM,
                                            format!(
                                                "Phase `{}`: `with.{key}` is not a parameter of `{callable}`{}",
                                                phase.key,
                                                super::markers::did_you_mean(
                                                    key,
                                                    params.iter().filter(|p| p.stars == 0).map(|p| p.name.as_str())
                                                )
                                            ),
                                            at("with"),
                                        ));
                                    }
                                }
                                let filled = |name: &str| {
                                    phase.with.contains_key(name)
                                        || WORKER_BUILTINS.contains(&name)
                                        || self
                                            .plugs
                                            .iter()
                                            .any(|pl| pl.key.eq_ignore_ascii_case(name))
                                        || phase
                                            .plug_aliases
                                            .keys()
                                            .any(|a| a.eq_ignore_ascii_case(name))
                                        || phase_keys.contains(name)
                                        || phase.sequence_path_str().is_some_and(|path| {
                                            phase_keys.contains(format!("{path}/{name}").as_str())
                                        })
                                };
                                for p in params
                                    .iter()
                                    .filter(|p| p.stars == 0 && !p.has_default && !filled(&p.name))
                                {
                                    if p.name == "vars" {
                                        problems.push(RefProblem::error(
                                            codes::PHASE_PARAM_RENAMED,
                                            format!(
                                                "Phase `{}`: parameter `vars` of `{callable}`: `vars` is now `config`",
                                                phase.key
                                            ),
                                            at("python"),
                                        ));
                                    } else if !phase.with.is_empty() {
                                        problems.push(RefProblem::error(
                                            codes::PHASE_MISSING_PARAM,
                                            format!(
                                                "Phase `{}`: missing `with.{}`, required by `{callable}`",
                                                phase.key, p.name
                                            ),
                                            at("with"),
                                        ));
                                    } else {
                                        // The run stops on it the same way
                                        // ("required parameter unresolved").
                                        let near = super::markers::did_you_mean(
                                            &p.name,
                                            self.plugs
                                                .iter()
                                                .map(|pl| pl.key.as_str())
                                                .chain(phase_keys.iter().copied())
                                                // What the worker injects
                                                // (`measurements`, `log`…).
                                                .chain(WORKER_BUILTINS.iter().copied())
                                                // A one- or two-letter key is
                                                // "near" every short name.
                                                .filter(|k| *k != phase.key && k.len() > 2),
                                        );
                                        problems.push(RefProblem::error(
                                            codes::PHASE_MISSING_PARAM,
                                            format!(
                                                "Phase `{}`: parameter `{}` of `{callable}` is not a plug key, a phase result nor a `with:` value{near}",
                                                phase.key, p.name
                                            ),
                                            at("python"),
                                        ));
                                    }
                                }
                            }
                            // A method called on a plug its class does not
                            // have fails the phase at run time
                            // (`Method x not found`). A warning: a class can
                            // gain methods in ways no reading of it sees.
                            if let Some(params) =
                                pysource::function_params_with_defaults(&source, &callable)
                            {
                                for (param, _) in &params {
                                    let key = phase
                                        .plug_aliases
                                        .iter()
                                        .find(|(a, _)| a.eq_ignore_ascii_case(param))
                                        .map(|(_, k)| k.as_str())
                                        .unwrap_or(param);
                                    let Some(plug) = self
                                        .plugs
                                        .iter()
                                        .find(|pl| pl.key.eq_ignore_ascii_case(key))
                                    else {
                                        continue;
                                    };
                                    let Some(members) = plug_class_members(plug, procedure_dir)
                                    else {
                                        continue;
                                    };
                                    let calls = pysource::calls_on(&source, &callable, param)
                                        .unwrap_or_default();
                                    // The proxy keeps `call` and `deadline`:
                                    // the driver's method of that name is
                                    // never reached from a phase.
                                    for method in calls.iter().filter(|m| {
                                        PLUG_PROXY_OWN.contains(&m.as_str()) && members.contains(*m)
                                    }) {
                                        problems.push(RefProblem {
                                            severity: RefSeverity::Warning,
                                            ..RefProblem::error(
                                                codes::PHASE_PLUG_METHOD_UNKNOWN,
                                                format!(
                                                    "Phase `{}`: `{param}.{method}()` calls the plug proxy's own `{method}`, not the method of plug `{}`; rename the driver method",
                                                    phase.key, plug.key
                                                ),
                                                at("python"),
                                            )
                                        });
                                    }
                                    for method in calls.into_iter().filter(|m| {
                                        !members.contains(m)
                                            && !PLUG_PROXY_OWN.contains(&m.as_str())
                                    }) {
                                        problems.push(RefProblem {
                                            severity: RefSeverity::Warning,
                                            ..RefProblem::error(
                                                codes::PHASE_PLUG_METHOD_UNKNOWN,
                                                format!(
                                                    "Phase `{}`: `{param}.{method}()` is not a method of plug `{}`, the phase would fail when it calls it{}",
                                                    phase.key,
                                                    plug.key,
                                                    super::markers::did_you_mean(
                                                        &method,
                                                        members.iter().map(String::as_str).filter(|m| !m.starts_with('_'))
                                                    )
                                                ),
                                                at("python"),
                                            )
                                        });
                                    }
                                }
                            }
                            // `None` = the body hands `ui` around and may
                            // fill the list somewhere this scan cannot see:
                            // stay quiet rather than call working code broken.
                            if let Some(filled) = pysource::ui_set_keys(&source, &callable) {
                                for (key, _) in unfilled
                                    .iter()
                                    .filter(|(k, _)| !filled.iter().any(|f| f == k))
                                {
                                    problems.push(unfilled_warning(
                                        key,
                                        &format!(
                                            "`{callable}` never calls `ui.set(\"{key}\", ...)`"
                                        ),
                                    ));
                                }
                            }
                            // A parameter naming a phase the worker will
                            // not hand over: the result is withheld unless
                            // the phase is visible (declared dependency,
                            // earlier stage, shared-to-shared), and a
                            // parameter without a default then fails the
                            // phase at start. One with a default receives
                            // the default: nothing to warn about.
                            // A `depends_on` naming no phase is refused on
                            // its own: the warning would only repeat it.
                            // A shared phase (`scope: execution`) runs once
                            // for every slot: a slot plug has one instance
                            // per slot and none it could take, and the run
                            // refuses it at start.
                            if let (Some(params), Some(stage)) = (
                                pysource::function_params_with_defaults(&source, &callable),
                                stage_of.get(phase.key.as_str()).copied(),
                            ) {
                                for (p, _) in params.iter().filter(|(_, has_default)| !has_default)
                                {
                                    if stage.is_shared()
                                        && self.plugs.iter().any(|pl| {
                                            pl.key.eq_ignore_ascii_case(p)
                                                && matches!(pl.scope, Scope::Slot)
                                        })
                                    {
                                        problems.push(RefProblem::error(
                                            codes::PHASE_PLUG_OUT_OF_SCOPE,
                                            format!(
                                                "Phase `{}`: plug `{p}` has one instance per slot and this phase runs once for all slots (`scope: execution`); give the plug `scope: execution`, or the phase `scope: slot`",
                                                phase.key
                                            ),
                                            at("python"),
                                        ));
                                    }
                                }
                            }
                            let dangling_dependency = phase
                                .depends_on
                                .iter()
                                .any(|d| !phase_keys.contains(d.as_str()));
                            if let (Some(params), Some(stage), false) = (
                                pysource::function_params_with_defaults(&source, &callable),
                                stage_of.get(phase.key.as_str()).copied(),
                                dangling_dependency,
                            ) {
                                let visible = self.visible_phase_keys(&phase.key, stage);
                                // A phase of a sequence reads a sibling by
                                // its short key, and the worker hands the
                                // sibling over before any procedure phase
                                // of the same name.
                                let sibling = |p: &str| {
                                    phase.sequence_path_str().is_some_and(|path| {
                                        phase_keys.contains(format!("{path}/{p}").as_str())
                                    })
                                };
                                for p in params
                                    .iter()
                                    .filter(|(p, has_default)| {
                                        !has_default
                                            && phase_keys.contains(p.as_str())
                                            && !sibling(p)
                                            && !visible.contains(p)
                                            && !WORKER_BUILTINS.contains(&p.as_str())
                                            && !self
                                                .plugs
                                                .iter()
                                                .any(|pl| pl.key.eq_ignore_ascii_case(p))
                                            // A sequence's plug requirement
                                            // (`plugs: {dmm: multimeter}`):
                                            // the worker hands it the plug.
                                            && !phase
                                                .plug_aliases
                                                .keys()
                                                .any(|k| k.eq_ignore_ascii_case(p))
                                    })
                                    .map(|(p, _)| p)
                                {
                                    let other = stage_of.get(p.as_str()).copied();
                                    let why = if stage.is_shared()
                                        && other.is_some_and(|o| !o.is_shared())
                                    {
                                        format!("{p} runs once per slot and a `scope: execution` phase only receives shared results; read it from a per-slot phase")
                                    } else if other.is_some_and(|o| o.rank() > stage.rank()) {
                                        format!("{p} runs later")
                                    } else {
                                        format!("add `depends_on: [{p}]`")
                                    };
                                    problems.push(RefProblem {
                                        // An error: the phase fails at start on
                                        // every run, so the run is refused first.
                                        code: codes::PHASE_RESULT_NOT_RECEIVED,
                                        severity: RefSeverity::Error,
                                        message: format!(
                                            "Phase `{}`: parameter `{p}` names phase {p}, whose result it does not receive, so the phase fails at start: {why}; or rename the parameter, or give it a default",
                                            phase.key
                                        ),
                                        location: at("python"),
                                        hint: None,
                                    });
                                }
                            }
                            let functions = pysource::top_level_functions(&source);
                            // A `def` the line scan sees but `binds_top_level`
                            // does not: a syntax error earlier in the file
                            // threw the scan off. The syntax error says so.
                            if !pysource::binds_top_level(&source, &callable)
                                && !functions.contains(&callable)
                            {
                                // The whole reference, as `hint` replaces the
                                // `python:` value; a sequence's phase was
                                // rewritten from its own file, which keeps
                                // the bare name.
                                let near = pysource::closest(
                                    &callable,
                                    functions.iter().map(String::as_str),
                                );
                                let hint = near
                                    .map(|n| format!("; did you mean `{n}`?"))
                                    .unwrap_or_default();
                                let mut problem = RefProblem::error(
                                    codes::PHASE_CALLABLE_NOT_FOUND,
                                    format!(
                                        "Phase `{}`: `{}` not found in {}{hint}",
                                        phase.key,
                                        callable,
                                        relative_to(&file, procedure_dir)
                                    ),
                                    at("python"),
                                );
                                // The whole reference, as `hint` replaces the
                                // `python:` value; a sequence's phase was
                                // rewritten from its own file (no hint there).
                                if phase.sequence_path.is_empty() {
                                    // Up to the last `:`: a drive letter
                                    // (`C:/proc/p.py:fn`) is part of the path.
                                    let module = match spec.as_str().rsplit_once(':') {
                                        Some((m, name)) if !name.contains(['/', '\\']) => m,
                                        _ => spec.as_str(),
                                    }
                                    .trim();
                                    problem.hint = near.map(|n| format!("{module}:{n}"));
                                }
                                problems.push(problem);
                            } else if let Some(recorded) =
                                pysource::recorded_measurement_names(&source, &callable)
                            {
                                // A warning: the phase may still record it
                                // at run time through a path this scan
                                // doesn't follow, and the run reports it
                                // either way.
                                let mut warned = HashSet::new();
                                for m in phase.measurements.iter().filter(|m| {
                                    !recorded.contains(&m.key)
                                        && !phase_binds_measurement(phase, &m.key)
                                        && warned.insert(m.key.as_str())
                                }) {
                                    // Same sentence the run-time matcher
                                    // logs, so the two never drift.
                                    let issue = MatchIssue::NotRecorded {
                                        measurement: m.key.clone(),
                                        part: DeclaredPart::Measurement,
                                    };
                                    // A name it records instead, declared
                                    // nowhere: the typo (`pwr` for `power`).
                                    let near = pysource::closest(
                                        &m.key,
                                        recorded.iter().map(String::as_str).filter(|r| {
                                            !phase.measurements.iter().any(|d| d.key == *r)
                                        }),
                                    )
                                    .map(|r| {
                                        format!("; the phase records `{r}`, which is not declared")
                                    })
                                    .unwrap_or_default();
                                    problems.push(RefProblem {
                                        code: codes::MEASUREMENT_NOT_RECORDED,
                                        severity: RefSeverity::Warning,
                                        message: format!("Phase `{}`: {issue}{near}", phase.key),
                                        location: at("measurements"),
                                        hint: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        problems.extend(self.missing_imports(procedure_dir, main_filter));
        problems
    }

    /// A top-level `from .x import name` (or an absolute import of a
    /// module of the procedure) in a phase or plug file that names
    /// something the module does not define: the import fails when the
    /// file loads, so does every phase of it. Once per file, at the first
    /// entry that uses it.
    fn missing_imports(
        &self,
        procedure_dir: &Path,
        main_filter: Option<&HashSet<String>>,
    ) -> Vec<RefProblem> {
        let mut entries: Vec<(PathBuf, String, RefLocation)> = Vec::new();
        if main_filter.is_none() {
            for (index, plug) in self.plugs.iter().enumerate() {
                if let Ok((file, _)) = plug.python.resolve_path(procedure_dir) {
                    entries.push((
                        file,
                        format!("Plug `{}`", plug.key),
                        RefLocation::key(RefList::Plugs, index, "python"),
                    ));
                }
            }
        }
        for (list, phases) in [
            (RefList::Setup, &self.setup),
            (RefList::Main, &self.main),
            (RefList::Teardown, &self.teardown),
        ] {
            for (index, phase) in phases.iter().enumerate() {
                let outside = list == RefList::Main
                    && main_filter.is_some_and(|set| !set.contains(&phase.key));
                if outside || phase.should_skip() {
                    continue;
                }
                let Some(spec) = &phase.python else { continue };
                if let Ok((file, _)) = spec.resolve_path(procedure_dir) {
                    entries.push((
                        file,
                        format!("Phase `{}`", phase.key),
                        self.written_location(list, index, "python"),
                    ));
                }
            }
        }
        let mut seen = HashSet::new();
        let mut problems = Vec::new();
        for (file, who, location) in entries {
            if !seen.insert(file.clone()) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (module, name) in unresolved_imports(&file, &source, procedure_dir) {
                // A warning: a module can bind names in ways no reading
                // of its source sees, and a run would be refused for it.
                let shown = relative_to(&file, procedure_dir);
                let message = if name.is_empty() {
                    format!("{who}: {shown} imports from `{module}`, a module the procedure does not have; the file would fail to load")
                } else {
                    format!("{who}: {shown} imports `{name}` from `{module}`, which does not seem to define it; the file would fail to load")
                };
                problems.push(RefProblem {
                    severity: RefSeverity::Warning,
                    ..RefProblem::error(codes::PYTHON_IMPORT_MISSING, message, location.clone())
                });
            }
        }
        problems
    }

    /// Get all phases as a flat list with stage, scope, and keys populated (for frontend serialization)
    pub fn get_flat_phases(&self) -> Vec<PhaseDefinition> {
        let mut phases = Vec::new();
        let mut seen_keys = HashSet::new();

        for (_, phase) in self.iter_phases_with_stage() {
            let mut p = phase.clone();
            let key = phase.key.clone();
            p.key = Self::ensure_unique_key(key, &mut seen_keys);
            if p.scope.is_none() {
                p.scope = Some(Scope::Slot);
            }
            phases.push(p);
        }

        phases
    }

    /// Get total phase count across all types
    pub fn total_phase_count(&self) -> usize {
        self.setup.len() + self.main.len() + self.teardown.len()
    }

    /// Get all plugs with their scopes
    pub fn get_all_plugs_with_scope(&self) -> Vec<(PlugScope, &PlugDefinition)> {
        self.plugs
            .iter()
            .map(|plug| {
                let scope = match plug.scope {
                    Scope::Execution => PlugScope::Execution,
                    Scope::Station => PlugScope::Station,
                    Scope::Slot => PlugScope::Slot,
                };
                (scope, plug)
            })
            .collect()
    }
}

impl PhaseDefinition {
    pub fn get_timeout_ms(&self) -> Option<u64> {
        self.timeout
    }
}

// Text styling enums live in `station-protocol` so the engine, the
// wire, and the operator-UI all use the same definitions. Re-exported
// here so YAML-side schemas keep their existing import path.
pub use station_protocol::{
    ComponentType as UIComponentType, ComponentValue, FontFamily, TextColor, TextSize,
};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "UPPERCASE")]
pub enum ValidatorOutcome {
    Pass,
    Fail,
    Unset,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ValidatorExpectedValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    NumberArray(Vec<f64>),
    StringArray(Vec<String>),
    MixedArray(Vec<serde_json::Value>),
    Object(serde_json::Map<String, serde_json::Value>),
}

/// The comparisons the evaluator knows how to run. A closed set on
/// purpose: the operator used to be a free string, so a typo (`=>`,
/// `gte`) loaded fine, evaluated to UNSET at run time, and the
/// measurement passed with its limit silently gone. As an enum the typo
/// is a YAML parse error with a line and column — refused by the loader
/// and underlined in Studio before the file is ever run.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ValidatorOperator {
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "not in")]
    NotIn,
    #[serde(rename = "matches")]
    Matches,
}

impl ValidatorOperator {
    /// The YAML spelling, which is also what the API and every display
    /// surface use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::In => "in",
            Self::NotIn => "not in",
            Self::Matches => "matches",
        }
    }
}

impl std::fmt::Display for ValidatorOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ValidatorSpec {
    /// Result of this validator on a recorded run; not written when authoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ValidatorOutcome>,
    /// Comparison: ==, !=, >, >=, <, <=, in, not in, matches (regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<ValidatorOperator>,
    /// What the operator compares the value against: a number, a string, or a list
    /// for in / not in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_value: Option<ValidatorExpectedValue>,
    /// Reserved for validators the phase code adds at run time; not written when
    /// authoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum AggregationValue {
    Number(f64),
    String(String),
    Boolean(bool),
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AggregationSpec {
    /// Name of a value the phase code sets on the measurement (mean, max, a
    /// count...). Nothing computes it: the phase must set it. Left unset, its
    /// validators are not evaluated and the phase does not fail on it, so a
    /// limit on an aggregation the code never sets is a limit that never
    /// fires.
    #[serde(rename = "type")]
    pub aggregation_type: String,
    /// Result on a recorded run; not written when authoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ValidatorOutcome>,
    /// Recorded aggregated value; not written when authoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<AggregationValue>,
    /// Unit of the aggregated value.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub unit: Option<String>,
    /// Limits on the aggregated value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum AxisData {
    Numeric(Vec<f64>),
    String(Vec<String>),
}

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AxisSpec {
    /// The recorded series; usually filled by the phase at run time, not written in
    /// the YAML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AxisData>,
    /// Unit of the series.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub unit: Option<String>,
    /// Legend label of the series.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub legend: Option<String>,
    /// Series key the phase writes to.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub key: Option<String>,
    /// Named values the phase code sets for this series; nothing computes
    /// them, and an unset one leaves its validators unevaluated (no failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,
    /// Limits on this series, judged against the whole series: only `==` and
    /// `!=` with the expected series judge it. Put a bound (`<=`, `>`) on an
    /// aggregation (`max`, `min`) instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
}

impl serde::Serialize for AxisSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Count non-None fields for struct size hint
        let mut count = 0;
        if self.data.is_some() {
            count += 1;
        }
        if self.get_key().is_some() {
            count += 1;
        }
        if self.get_legend().is_some() {
            count += 1;
        }
        if self.unit.is_some() {
            count += 1;
        }
        if self.aggregations.is_some() {
            count += 1;
        }
        if self.validators.is_some() {
            count += 1;
        }
        if self.description.is_some() {
            count += 1;
        }

        let mut state = serializer.serialize_struct("AxisSpec", count)?;
        if let Some(ref data) = self.data {
            state.serialize_field("data", data)?;
        }
        if let Some(ref unit) = self.unit {
            state.serialize_field("unit", unit)?;
        }
        // Always serialize resolved legend and key
        if let Some(legend) = self.get_legend() {
            state.serialize_field("legend", &legend)?;
        }
        if let Some(key) = self.get_key() {
            state.serialize_field("key", &key)?;
        }
        if let Some(ref aggs) = self.aggregations {
            state.serialize_field("aggregations", aggs)?;
        }
        if let Some(ref vals) = self.validators {
            state.serialize_field("validators", vals)?;
        }
        if let Some(ref desc) = self.description {
            state.serialize_field("description", desc)?;
        }
        state.end()
    }
}

impl AxisSpec {
    /// Returns the explicit key, or auto-generates one from legend via to_python_identifier
    pub fn get_key(&self) -> Option<String> {
        if let Some(ref key) = self.key {
            if !key.is_empty() {
                return Some(key.clone());
            }
        }
        if let Some(ref legend) = self.legend {
            let generated = crate::python::to_python_identifier(legend);
            if !generated.is_empty() {
                return Some(generated);
            }
        }
        None
    }

    /// Returns the explicit legend, or auto-generates one from key (title-cased)
    pub fn get_legend(&self) -> Option<String> {
        if let Some(ref legend) = self.legend {
            if !legend.is_empty() {
                return Some(legend.clone());
            }
        }
        if let Some(ref key) = self.key {
            if !key.is_empty() {
                // Title-case: "voltage_output" -> "Voltage Output"
                return Some(
                    key.split('_')
                        .filter(|s| !s.is_empty())
                        .map(|word| {
                            let mut chars = word.chars();
                            match chars.next() {
                                Some(c) => {
                                    c.to_uppercase().to_string() + &chars.collect::<String>()
                                }
                                None => String::new(),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        None
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MultiDimensionalSpec {
    pub x_axis: AxisSpec,
    pub y_axis: Vec<AxisSpec>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub title: Option<String>,
}

// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct MeasurementSpecYaml {
    /// Measurement key, a Python identifier; derived from name when omitted. The
    /// phase records it as measurements.<key> = value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Display name of the measurement.
    #[serde(deserialize_with = "trimmed_text")]
    pub name: String,
    /// Physical unit shown with the value, e.g. V, mA.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub unit: Option<String>,
    /// Pass/fail limits on the recorded value. A lower and an upper bound are two
    /// validators. Not allowed on a multi-dimensional measurement (one with
    /// `x_axis`): put its limits on an axis or an aggregation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "judgeable_validators"
    )]
    pub validators: Option<Vec<ValidatorSpec>>,
    /// Named values the phase code sets on a multi-dimensional measurement,
    /// each with its own validators. Nothing computes them: an aggregation
    /// the phase never sets leaves its validators unevaluated, it does not
    /// fail.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "judgeable_aggregations"
    )]
    pub aggregations: Option<Vec<AggregationSpec>>,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
    /// Chart title of a multi-dimensional measurement.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub title: Option<String>,
    /// X axis of a multi-dimensional measurement (a series recorded as a list).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "judgeable_axis"
    )]
    pub x_axis: Option<AxisSpec>,
    /// Y axes of a multi-dimensional measurement, one per series.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "judgeable_axes"
    )]
    pub y_axis: Option<Vec<AxisSpec>>,
}

/// A validator a measurement declares, refused while parsing when its
/// limit can never judge a value (`limit_problem`). Refusing it there
/// gives the error the validator's line and column; at run time the same
/// validator would read FAIL on every run.
struct JudgeableValidator(ValidatorSpec);

impl<'de> Deserialize<'de> for JudgeableValidator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let validator = ValidatorSpec::deserialize(deserializer)?;
        match crate::measurements::evaluation::limit_problem(&validator) {
            Some(problem) => Err(serde::de::Error::custom(problem)),
            None => Ok(Self(validator)),
        }
    }
}

/// An aggregation a measurement declares, refused while parsing when one
/// of its validators can never judge a value.
struct JudgeableAggregation(AggregationSpec);

impl<'de> Deserialize<'de> for JudgeableAggregation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let aggregation = AggregationSpec::deserialize(deserializer)?;
        match aggregation_problem(&aggregation) {
            Some(problem) => Err(serde::de::Error::custom(problem)),
            None => Ok(Self(aggregation)),
        }
    }
}

fn aggregation_problem(aggregation: &AggregationSpec) -> Option<String> {
    aggregation
        .validators
        .iter()
        .flatten()
        .enumerate()
        .find_map(|(index, validator)| {
            crate::measurements::evaluation::limit_problem(validator)
                .map(|problem| format!("validators[{index}]: {problem}"))
        })
}

/// A measurement, refused while parsing when it declares `x_axis` and
/// measurement-level `validators`: its reading is a set of series, which
/// such a limit can't judge (`MatchIssue::LimitOnSeries` at run time).
struct JudgeableMeasurement(MeasurementSpecYaml);

impl<'de> Deserialize<'de> for JudgeableMeasurement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let m = MeasurementSpecYaml::deserialize(deserializer)?;
        if m.x_axis.is_some() && m.validators.as_ref().is_some_and(|v| !v.is_empty()) {
            return Err(serde::de::Error::custom(
                "validators: not allowed with `x_axis`; move them to `x_axis.validators`, \
                 `y_axis[].validators` or `aggregations`",
            ));
        }
        Ok(Self(m))
    }
}

fn judgeable_measurements<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MeasurementSpecYaml>, D::Error> {
    let measurements = Vec::<JudgeableMeasurement>::deserialize(deserializer)?;
    Ok(measurements.into_iter().map(|m| m.0).collect())
}

/// `judgeable_measurements` for an optional list (the phase-or-call entry).
fn judgeable_measurements_opt<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<MeasurementSpecYaml>>, D::Error> {
    let measurements = Option::<Vec<JudgeableMeasurement>>::deserialize(deserializer)?;
    Ok(measurements.map(|list| list.into_iter().map(|m| m.0).collect()))
}

/// An axis a measurement declares, refused while parsing when one of its
/// validators, or of its aggregations', can never judge a value. Axis
/// validators follow the axis rule (`axis_limit_problem`): an operator
/// other than `==` / `!=` loads, with a warning, and stays UNSET.
struct JudgeableAxis(AxisSpec);

impl<'de> Deserialize<'de> for JudgeableAxis {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let axis = AxisSpec::deserialize(deserializer)?;
        for (index, validator) in axis.validators.iter().flatten().enumerate() {
            if let Some(problem) = crate::measurements::evaluation::axis_limit_problem(validator) {
                return Err(serde::de::Error::custom(format!(
                    "validators[{index}]: {problem}"
                )));
            }
        }
        for (index, aggregation) in axis.aggregations.iter().flatten().enumerate() {
            if let Some(problem) = aggregation_problem(aggregation) {
                return Err(serde::de::Error::custom(format!(
                    "aggregations[{index}].{problem}"
                )));
            }
        }
        Ok(Self(axis))
    }
}

fn judgeable_axis<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AxisSpec>, D::Error> {
    let axis = Option::<JudgeableAxis>::deserialize(deserializer)?;
    Ok(axis.map(|a| a.0))
}

fn judgeable_axes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<AxisSpec>>, D::Error> {
    let axes = Option::<Vec<JudgeableAxis>>::deserialize(deserializer)?;
    Ok(axes.map(|a| a.into_iter().map(|a| a.0).collect()))
}

fn judgeable_validators<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<ValidatorSpec>>, D::Error> {
    let validators = Option::<Vec<JudgeableValidator>>::deserialize(deserializer)?;
    Ok(validators.map(|v| v.into_iter().map(|v| v.0).collect()))
}

fn judgeable_aggregations<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<AggregationSpec>>, D::Error> {
    let aggregations = Option::<Vec<JudgeableAggregation>>::deserialize(deserializer)?;
    Ok(aggregations.map(|a| a.into_iter().map(|a| a.0).collect()))
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MeasurementSpec {
    #[validate(
        length(min = 1, max = 100),
        custom(function = "validate_python_identifier")
    )]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[validate(length(max = 50))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[validate(length(max = 200))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_axis: Option<AxisSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<Vec<AxisSpec>>,
}

impl From<MeasurementSpecYaml> for MeasurementSpec {
    fn from(yaml: MeasurementSpecYaml) -> Self {
        let trimmed = yaml.name.trim();
        let name = if trimmed.is_empty() {
            yaml.name
        } else {
            trimmed.to_string()
        };
        let key = yaml
            .key
            .unwrap_or_else(|| crate::python::to_python_identifier(&name));

        MeasurementSpec {
            key,
            name,
            unit: yaml.unit,
            validators: yaml.validators,
            aggregations: yaml.aggregations,
            description: yaml.description,
            title: yaml.title,
            x_axis: yaml.x_axis,
            y_axis: yaml.y_axis,
        }
    }
}

impl MeasurementSpec {
    pub fn to_yaml(&self) -> MeasurementSpecYaml {
        let auto_key = crate::python::to_python_identifier(&self.name);
        MeasurementSpecYaml {
            key: if self.key != auto_key {
                Some(self.key.clone())
            } else {
                None
            },
            name: self.name.clone(),
            unit: self.unit.clone(),
            validators: self.validators.clone(),
            aggregations: self.aggregations.clone(),
            description: self.description.clone(),
            title: self.title.clone(),
            x_axis: self.x_axis.clone(),
            y_axis: self.y_axis.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for MeasurementSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = JudgeableMeasurement::deserialize(deserializer)?.0;
        Ok(yaml.into())
    }
}

/// The procedure's root `ui:`: what the operator sees for the whole run.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProcedureUiConfig {
    /// Run panel .html, same rules as a phase's ui.panel: one <slot></slot> shows
    /// each phase's screen; only buttons <button name="stop"> and name="kill".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Default, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UIConfig {
    /// Components of the operator screen, top to bottom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    #[cfg_attr(
        feature = "json-schema",
        schemars(with = "Option<Vec<UIComponentYaml>>")
    )]
    pub components: Option<Vec<UIComponent>>,

    /// true adds a Continue gate to a display-only screen. A screen with any
    /// input component always waits for the operator, whatever this says;
    /// false changes nothing (a display-only screen does not wait by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_input: Option<bool>,

    /// Custom screen: an .html file (inline <svg>, <style>; no JavaScript). data-bind=
    /// "PATH" sets data-value, numeric --value, a leaf's text, and on a measurement
    /// data-outcome; data-format="0.0". PATH: ui.KEY (a component, or any ui.KEY = v
    /// the phase writes), measurements.KEY[.outcome|units], unit.*, run.status.
    /// <input name="KEY"> edits input component KEY; <slot name="KEY"> places it;
    /// <button name="X"> is read with ui.pressed("X"); <button type="submit"
    /// name="KEY" value="V"> writes V then answers. Show/hide with CSS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UIComponentYaml {
    // Core Identity
    /// Component type: text_input, number_input, textarea, switch, radio, select,
    /// multiselect, checklist, slider (inputs) or text, image, progress (display).
    #[serde(rename = "type")]
    component_type: UIComponentType,
    /// Unique within the phase: the name the phase writes through ui.<key> and the
    /// name of the recorded answer. Derived from label, else from the type, when
    /// omitted: always write it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    // Display/UI
    /// Heading drawn above the component. On a text component it is usually
    /// unwanted: the text itself goes in default_value.
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    label: Option<String>,
    /// Helper text under the label.
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    description: Option<String>,
    /// Placeholder shown inside an empty input.
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    placeholder: Option<String>,

    // Behavior
    /// The operator must answer before submitting. Default false.
    #[serde(default)]
    required: bool,
    /// Where the answer goes: measurements.<key> (declared on the same phase),
    /// unit.serial_number|part_number|revision_number|batch_number|sub_units.<key>,
    /// or run.operated_by. Not unit metadata (set it from phase code). A unit
    /// identity target only accepts letters, digits and _ . : + - (affixes included).
    #[serde(default)]
    bind: Option<String>,
    /// Initial value of an input; on text and image, the content to display.
    #[serde(default)]
    default_value: Option<ComponentValue>,

    // Text Constraints
    /// Minimum length of the typed text.
    #[serde(default)]
    min_length: Option<u32>,
    /// Maximum length of the typed text.
    #[serde(default)]
    max_length: Option<u32>,
    /// Regex the typed text must match in full (anchors change nothing).
    #[serde(default)]
    pattern: Option<String>,
    /// Error shown when the text does not match pattern.
    #[serde(default)]
    pattern_message: Option<String>,
    /// Fixed text before a text_input's answer: the recorded value is prefix +
    /// input + suffix, the pattern validates only the typed part. On number_input,
    /// a display-only unit label.
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    prefix: Option<String>,
    /// Fixed text after a text_input's answer (see prefix). On number_input, a
    /// display-only unit label.
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    suffix: Option<String>,
    /// Trim surrounding whitespace from the answer. Default true.
    #[serde(default = "default_true")]
    trim: bool,

    // Textarea-specific
    /// Visible rows of a textarea.
    #[serde(default)]
    rows: Option<u32>,

    // Number Constraints
    /// Lower bound of a number_input, a slider or a progress bar.
    #[serde(default)]
    min: Option<f64>,
    /// Upper bound of a number_input, a slider or a progress bar.
    #[serde(default)]
    max: Option<f64>,
    /// Step of a number_input or slider.
    #[serde(default)]
    step: Option<f64>,

    // Select/Choice Options
    /// Choices of a radio, select, multiselect or checklist: [{value, label,
    /// image?}] and nothing else. On a switch, its two labels. Leave it out
    /// when the choices are only known at run time and fill them from the
    /// phase with `ui.set("<key>", options=[...])`: the screen shows
    /// "Waiting for options" until they arrive, and the operator's answer is
    /// checked against the list the phase sent.
    #[serde(default)]
    options: Option<Vec<SelectOption>>,

    // Grid columns (for radio/checklist rendered as image grids)
    /// Grid columns when the options carry images.
    #[serde(default)]
    columns: Option<u32>,

    // Image/Layout
    /// Width of an image, or of an image grid's cells.
    #[serde(default)]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<PercentText>"))]
    width: Option<String>,
    /// CSS height of an image, or of an image grid's cells, applied as
    /// written (no check at load).
    #[serde(default)]
    height: Option<String>,
    /// Aspect ratio of an image, or of an image grid's cells: one of the
    /// listed values, nothing else loads.
    #[serde(default)]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<ImageAspectText>"))]
    aspect: Option<String>,
    /// How an image fills its box: one of the listed values, nothing else
    /// loads.
    #[serde(default)]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<ImageFitText>"))]
    fit: Option<String>,

    // Text Styling
    /// Text size of a text component.
    #[serde(default)]
    size: Option<TextSize>,
    /// Text color of a text component.
    #[serde(default)]
    color: Option<TextColor>,
    /// Font family of a text component.
    #[serde(default)]
    font: Option<FontFamily>,
}

/// The key a component gets when its file states none: from the label,
/// else from the type name. A label with nothing an identifier can keep
/// (`"!!!"`) falls through to the type too — it used to derive `""`,
/// which failed validation and took the whole procedure down with it.
fn derived_component_key(component_type: &UIComponentType, label: Option<&str>) -> String {
    label
        .map(crate::python::to_python_identifier)
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| crate::python::to_python_identifier(&format!("{component_type:?}")))
}

impl From<UIComponentYaml> for UIComponent {
    fn from(yaml: UIComponentYaml) -> Self {
        let key_stated = yaml.key.is_some();
        let key = yaml
            .key
            .unwrap_or_else(|| derived_component_key(&yaml.component_type, yaml.label.as_deref()));

        UIComponent {
            // Core Identity
            component_type: yaml.component_type,
            key,
            key_stated,

            // Display/UI
            label: yaml.label,
            description: yaml.description,
            placeholder: yaml.placeholder,

            // Behavior
            required: yaml.required,
            bind: yaml.bind,
            default_value: yaml.default_value,

            // Text Constraints
            min_length: yaml.min_length,
            max_length: yaml.max_length,
            pattern: yaml.pattern,
            pattern_message: yaml.pattern_message,
            prefix: yaml.prefix,
            suffix: yaml.suffix,
            trim: yaml.trim,
            rows: yaml.rows,

            // Number Constraints
            min: yaml.min,
            max: yaml.max,
            step: yaml.step,
            // Select/Choice Options — normalize Some([]) to None so downstream
            // consumers can treat "no options" as a single, consistent shape.
            options: yaml.options.filter(|v| !v.is_empty()),

            // Grid columns
            columns: yaml.columns,

            // Image/Layout
            width: yaml.width,
            height: yaml.height,
            aspect: yaml.aspect,
            fit: yaml.fit,

            // Text Styling
            size: yaml.size,
            color: yaml.color,
            font: yaml.font,
        }
    }
}

#[derive(Debug, Serialize, Validate, Clone)]
pub struct UIComponent {
    // Core Identity
    #[serde(rename = "type")]
    pub component_type: UIComponentType,

    #[validate(
        length(min = 1, max = 100),
        custom(function = "validate_python_identifier")
    )]
    pub key: String,

    /// Whether the file wrote `key:`, or `key` above was derived. Noted at
    /// the one point where both are known, since `key` alone cannot tell.
    /// Never serialized: it describes the file, for the Studio Inspector,
    /// and is no part of the component anything downstream receives.
    #[serde(skip)]
    pub key_stated: bool,

    // Display/UI
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 1000))]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub placeholder: Option<String>,

    // Behavior
    #[serde(default = "default_true")]
    pub required: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub bind: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<ComponentValue>,

    // Text Constraints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub pattern: Option<String>,
    /// Error message shown when the value does not match `pattern`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub pattern_message: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub suffix: Option<String>,
    #[serde(default = "default_true")]
    pub trim: bool,

    // Textarea-specific
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,

    // Number Constraints
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub step: Option<f64>,

    // Select/Choice Options
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub options: Option<Vec<SelectOption>>,

    // Grid columns (for radio/checklist rendered as image grids)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,

    // Image/Layout
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub width: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub height: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub aspect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub fit: Option<String>,

    // Text Styling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<TextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<TextColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontFamily>,
}

impl UIComponent {
    /// Fields the file states that nothing reads on this component type:
    /// the line loads, renders nothing and validates nothing, so a
    /// `color:` on a text input looks like a bug in the operator UI.
    ///
    /// Which type reads which field is NOT decided here: that is
    /// `station_protocol::COMPONENT_FIELD_READERS`, the one table, shared
    /// with the web. This only says which of those fields the file
    /// stated, and `inert_fields_cover_the_reader_table` fails if the two
    /// lists ever name different fields.
    pub fn inert_fields(&self) -> Vec<&'static str> {
        self.stated_type_specific_fields()
            .into_iter()
            .filter(|(name, stated)| *stated && !self.component_type.reads(name))
            .map(|(name, _)| name)
            .collect()
    }

    /// Every field of the reader table, with whether the file states it.
    fn stated_type_specific_fields(&self) -> [(&'static str, bool); 20] {
        [
            ("placeholder", self.placeholder.is_some()),
            ("min_length", self.min_length.is_some()),
            ("max_length", self.max_length.is_some()),
            ("pattern", self.pattern.is_some()),
            ("pattern_message", self.pattern_message.is_some()),
            ("prefix", self.prefix.is_some()),
            ("suffix", self.suffix.is_some()),
            ("rows", self.rows.is_some()),
            ("min", self.min.is_some()),
            ("max", self.max.is_some()),
            ("step", self.step.is_some()),
            ("options", self.options.is_some()),
            ("columns", self.columns.is_some()),
            ("width", self.width.is_some()),
            ("height", self.height.is_some()),
            ("aspect", self.aspect.is_some()),
            ("fit", self.fit.is_some()),
            ("size", self.size.is_some()),
            ("color", self.color.is_some()),
            ("font", self.font.is_some()),
        ]
    }

    pub fn to_yaml(&self) -> UIComponentYaml {
        let auto_key = derived_component_key(&self.component_type, self.label.as_deref());

        UIComponentYaml {
            component_type: self.component_type,
            key: if self.key != auto_key {
                Some(self.key.clone())
            } else {
                None
            },
            label: self.label.clone(),
            description: self.description.clone(),
            placeholder: self.placeholder.clone(),
            required: self.required,
            bind: self.bind.clone(),
            default_value: self.default_value.clone(),
            min_length: self.min_length,
            max_length: self.max_length,
            pattern: self.pattern.clone(),
            pattern_message: self.pattern_message.clone(),
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
            trim: self.trim,
            rows: self.rows,
            min: self.min,
            max: self.max,
            step: self.step,
            options: self.options.clone(),
            columns: self.columns,
            width: self.width.clone(),
            height: self.height.clone(),
            aspect: self.aspect.clone(),
            fit: self.fit.clone(),
            size: self.size,
            color: self.color,
            font: self.font,
        }
    }

    pub fn validate_width(&self) -> Result<(), String> {
        if let Some(ref w) = self.width {
            let trimmed = w.trim();
            if !trimmed.ends_with('%') {
                return Err(format!(
                    "width: invalid `{w}`: expected a percentage, e.g. `50%`"
                ));
            }
            let num_part = &trimmed[..trimmed.len() - 1];
            match num_part.parse::<f64>() {
                Ok(v) if v > 0.0 && v <= 100.0 => {}
                _ => {
                    return Err(format!("width: invalid `{w}`: expected 0%-100%"));
                }
            }
        }
        Ok(())
    }

    pub fn validate_aspect(&self) -> Result<(), String> {
        if let Some(ref a) = self.aspect {
            let trimmed = a.trim();
            if trimmed.is_empty() {
                return Ok(());
            }
            if !IMAGE_ASPECTS.contains(&trimmed) {
                return Err(format!(
                    "aspect: invalid `{a}`: expected {}",
                    IMAGE_ASPECTS
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(())
    }

    pub fn validate_fit(&self) -> Result<(), String> {
        if let Some(ref f) = self.fit {
            let trimmed = f.trim();
            if trimmed.is_empty() {
                return Ok(());
            }
            if !IMAGE_FITS.contains(&trimmed) {
                return Err(format!(
                    "fit: invalid `{f}`: expected {}",
                    IMAGE_FITS
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Load-time twin of `validate_unit_field_affixes` for a phase
    /// `text_input` bound to a unit identity field: its `prefix` /
    /// `suffix` compose into the recorded value, so a character the
    /// server rejects would only surface at upload, after the test ran.
    pub fn validate_identity_affixes(&self) -> Result<(), String> {
        if self.component_type != UIComponentType::TextInput {
            return Ok(());
        }
        let Some(bind) = self.bind.as_deref() else {
            return Ok(());
        };
        if !binds_unit_identity(bind) {
            return Ok(());
        }
        for (name, affix) in [
            ("prefix", self.prefix.as_deref()),
            ("suffix", self.suffix.as_deref()),
        ] {
            let Some(affix) = affix else { continue };
            if !affix.is_empty() && !is_identity_charset(affix) {
                return Err(format!(
                    "{name}: invalid `{affix}` for `bind: {bind}`: expected A-Z a-z 0-9 _ . : + -"
                ));
            }
        }
        Ok(())
    }

    pub fn validate_options_count(&self) -> Result<(), String> {
        // Options with images render as a card grid; cap it so the grid
        // stays usable on operator screens. Plain text lists are unlimited.
        // The cap is shared with run-time lists (`ui.set`), which the
        // station-protocol crate checks with the same constant.
        use station_protocol::properties::MAX_OPTIONS_WITH_IMAGES;
        if let Some(opts) = &self.options {
            let has_images = opts.iter().any(|o| o.image.is_some());
            if has_images && opts.len() > MAX_OPTIONS_WITH_IMAGES {
                return Err(format!(
                    "options: expected at most {MAX_OPTIONS_WITH_IMAGES} with images, got {}",
                    opts.len()
                ));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for UIComponent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = UIComponentYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

#[derive(Debug, Deserialize, Serialize, Validate, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    /// Recorded value of the choice.
    #[validate(length(max = 200))]
    #[serde(deserialize_with = "trimmed_text")]
    pub value: String,
    /// Text shown to the operator.
    #[validate(length(max = 200))]
    #[serde(deserialize_with = "trimmed_text")]
    pub label: String,
    /// Image URL or path shown with the choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub image: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Scope spellings: slot/execution/station only
    // ========================================================================

    #[test]
    fn scope_parses_its_spellings() {
        for (input, expected) in [
            ("slot", Scope::Slot),
            ("execution", Scope::Execution),
            ("station", Scope::Station),
        ] {
            let parsed: Scope = crate::procedure::yaml::from_str(input)
                .unwrap_or_else(|e| panic!("`{input}` should parse: {e}"));
            assert_eq!(parsed, expected, "`{input}`");
        }
    }

    #[test]
    fn scope_rejects_unknown_spelling() {
        assert!(crate::procedure::yaml::from_str::<Scope>("global").is_err());
    }

    #[test]
    fn scope_emits_new_spellings() {
        for (scope, expected) in [
            (Scope::Slot, "slot"),
            (Scope::Execution, "execution"),
            (Scope::Station, "station"),
        ] {
            let out = crate::procedure::yaml::to_string(&scope);
            assert_eq!(out.trim(), expected);
        }
    }

    #[test]
    fn plug_scope_defaults_to_slot_and_roundtrip_omits_default() {
        let yaml = r#"
name: Bare Plug
python: instruments.psu:PowerSupply
"#;
        let plug: PlugDefinition = crate::procedure::yaml::from_str(yaml).unwrap();
        assert_eq!(plug.scope, Scope::Slot);

        // Default scope must not be written back — Studio re-saves whole
        // files and a procedure that never specified a scope must stay
        // byte-identical.
        let back = plug.to_yaml();
        assert!(back.scope.is_none());
    }

    #[test]
    fn plug_station_scope_roundtrips() {
        let yaml = r#"
name: Power Supply
python: instruments.psu:PowerSupply
scope: station
"#;
        let plug: PlugDefinition = crate::procedure::yaml::from_str(yaml).unwrap();
        assert_eq!(plug.scope, Scope::Station);
        assert!(plug.scope_is_station());
        assert!(!plug.scope_is_execution());

        let back = plug.to_yaml();
        assert_eq!(back.scope, Some(Scope::Station));
    }

    #[test]
    fn plugs_with_scope_maps_station() {
        let yaml = r#"
name: Test
version: "1.0"
plugs:
  - name: Sta
    python: instruments.a:A
    scope: station
  - name: Shared
    python: instruments.b:B
    scope: execution
  - name: Per Slot
    python: instruments.c:C
main:
  - key: p1
    name: P1
    python: phases.p1
"#;
        let raw: ProcedureYaml = crate::procedure::yaml::from_str(yaml).unwrap();
        let def = ProcedureDefinition::try_from(raw).unwrap();
        let scopes: Vec<_> = def
            .get_all_plugs_with_scope()
            .into_iter()
            .map(|(s, p)| (p.key.clone(), s))
            .collect();
        assert_eq!(
            scopes,
            vec![
                ("sta".to_string(), crate::events::PlugScope::Station),
                ("shared".to_string(), crate::events::PlugScope::Execution),
                ("per_slot".to_string(), crate::events::PlugScope::Slot),
            ]
        );
    }

    // ========================================================================
    // Unit metadata tests
    // ========================================================================

    #[test]
    fn unit_metadata_field_configs_parse() {
        let yaml = r#"
name: Test
version: "1.0"
unit:
  serial_number:
    pattern: "^SN-"
  part_number: {}
  metadata:
    modification:
      placeholder: "MOD-42"
      pattern: "^MOD-[0-9]+$"
    amendment: {}
main:
  - key: p1
    name: P1
    python: phases.p1
"#;
        let raw: ProcedureYaml = crate::procedure::yaml::from_str(yaml).unwrap();
        let unit = raw.unit.expect("unit parsed");
        let md = unit.metadata.expect("unit metadata parsed");
        assert_eq!(md.len(), 2);
        assert_eq!(
            md.get("modification").unwrap().pattern.as_deref(),
            Some("^MOD-[0-9]+$")
        );
        assert!(md.get("amendment").unwrap().pattern.is_none());
    }

    #[test]
    fn validate_metadata_keys_rules() {
        // valid
        assert!(validate_metadata_keys(["mod-1", "a.b:c+d_e"].into_iter(), "metadata").is_ok());
        // bad charset
        assert!(validate_metadata_keys(["has space"].into_iter(), "metadata").is_err());
        // empty
        assert!(validate_metadata_keys([""].into_iter(), "metadata").is_err());
        // too long
        let long = "k".repeat(41);
        assert!(validate_metadata_keys([long.as_str()].into_iter(), "metadata").is_err());
        // too many
        let many: Vec<String> = (0..51).map(|i| format!("k{i}")).collect();
        assert!(validate_metadata_keys(many.iter().map(|s| s.as_str()), "metadata").is_err());
    }

    #[test]
    fn validate_unit_field_affix_rules() {
        let with_prefix = |p: &str| UnitFieldConfig {
            prefix: Some(p.to_string()),
            ..Default::default()
        };
        // valid, charset-restricted
        assert!(
            validate_unit_field_affixes("unit.serial_number", &with_prefix("SN-"), true).is_ok()
        );
        // charset violation only rejected on identity fields
        assert!(
            validate_unit_field_affixes("unit.serial_number", &with_prefix("SN "), true).is_err()
        );
        assert!(validate_unit_field_affixes(
            "unit.metadata.note",
            &with_prefix("has space "),
            false
        )
        .is_ok());
        // empty and over-long rejected everywhere
        assert!(validate_unit_field_affixes("unit.serial_number", &with_prefix(""), true).is_err());
        let long = "p".repeat(51);
        assert!(
            validate_unit_field_affixes("unit.serial_number", &with_prefix(&long), true).is_err()
        );
        // suffix checked with the same rules
        let bad_suffix = UnitFieldConfig {
            suffix: Some("@eu".to_string()),
            ..Default::default()
        };
        assert!(validate_unit_field_affixes("unit.serial_number", &bad_suffix, true).is_err());
        assert!(validate_unit_field_affixes("operated_by", &bad_suffix, false).is_ok());
    }

    #[test]
    fn bound_text_input_affixes_get_the_identity_charset_check() {
        let comp = |bind: &str, prefix: &str| -> UIComponent {
            crate::procedure::yaml::from_str(&format!(
                "key: sn\ntype: text_input\nbind: {bind}\nprefix: \"{prefix}\"\n"
            ))
            .unwrap()
        };
        // Component affixes are trimmed at parse, so the violation here
        // is a character the server rejects, not a stray space.
        assert!(comp("unit.serial_number", "SN-")
            .validate_identity_affixes()
            .is_ok());
        let err = comp("unit.serial_number", "SN#")
            .validate_identity_affixes()
            .unwrap_err();
        assert!(err.starts_with("prefix: invalid `SN#`"), "got: {err}");
        assert!(comp("unit.sub_units.battery", "BAT/")
            .validate_identity_affixes()
            .is_err());
        // Free text server-side: no charset rule.
        assert!(comp("unit.metadata.note", "note#")
            .validate_identity_affixes()
            .is_ok());
        assert!(comp("run.operated_by", "Dr.")
            .validate_identity_affixes()
            .is_ok());
        assert!(comp("measurements.board", "PCB/")
            .validate_identity_affixes()
            .is_ok());
        // Only text inputs compose.
        let mut num = comp("unit.serial_number", "SN#");
        num.component_type = UIComponentType::NumberInput;
        assert!(num.validate_identity_affixes().is_ok());
    }

    #[test]
    fn unit_config_validate_affixes_covers_all_fields() {
        let mut cfg = UnitConfig::default();
        assert!(cfg.validate_affixes().is_ok());
        cfg.sub_units = Some(SubUnitsConfig(vec![SubUnitItemConfig {
            label: "Battery".to_string(),
            key: Some("battery".to_string()),
            serial_number: Some(UnitFieldConfig {
                prefix: Some("BAT ".to_string()), // space → charset violation
                ..Default::default()
            }),
        }]));
        let err = cfg.validate_affixes().unwrap_err();
        assert!(
            err.starts_with("unit.sub_units[0].serial_number.prefix"),
            "got: {err}"
        );
    }

    #[test]
    fn test_plug_config_parses_and_roundtrips() {
        let yaml = r#"
key: rigol_a
name: Rigol A
python: instruments.rigol:RigolDP832
config:
  address: "192.168.1.100"
  channel: 1
"#;
        let plug: PlugDefinition =
            crate::procedure::yaml::from_str(yaml).expect("plug with config should parse");

        let config = plug.with.as_ref().expect("config should be present");
        assert_eq!(config.get("address").unwrap(), "192.168.1.100");
        assert_eq!(config.get("channel").unwrap(), 1);

        // config survives the roundtrip back to the YAML representation
        let back = plug.to_yaml();
        assert_eq!(
            back.config.as_ref().and_then(|c| c.get("channel")),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn test_plug_without_config_is_none() {
        let yaml = r#"
name: Power Supply
python: instruments.power_supply:PowerSupply
"#;
        let plug: PlugDefinition =
            crate::procedure::yaml::from_str(yaml).expect("plug without config should parse");
        assert!(plug.with.is_none());
    }

    #[test]
    fn init_timeout_reaches_the_plug_service_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("rigol.py"), "class Rigol:\n    pass\n")
            .expect("write plug file");
        let plug: PlugDefinition = crate::procedure::yaml::from_str(
            "name: Rigol\npython: rigol:Rigol\ninit_timeout: 2m\n",
        )
        .expect("parse plug with init_timeout");
        assert_eq!(plug.init_timeout, Some(120_000));
        let json = plug.to_config_json(dir.path()).expect("to_config_json");
        assert_eq!(json["init_timeout_ms"], 120_000);
        assert!(json["config"].is_null(), "never handed to the class");

        let plain: PlugDefinition =
            crate::procedure::yaml::from_str("name: Rigol\npython: rigol:Rigol\n").unwrap();
        let json = plain.to_config_json(dir.path()).unwrap();
        assert!(json.get("init_timeout_ms").is_none());
    }

    #[test]
    fn test_to_config_json_emits_config() {
        // to_config_json feeds the plug runtime; assert the config it emits is
        // exactly the parsed mapping. PythonSpec::parse checks the file exists,
        // so back the spec with a real file in a temp project dir.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("rigol.py"), "class Rigol:\n    pass\n")
            .expect("write plug file");

        let with_config: PlugDefinition = crate::procedure::yaml::from_str(
            "name: Rigol\npython: rigol:Rigol\nconfig:\n  address: \"10.0.0.1\"\n  channel: 2\n",
        )
        .expect("parse plug with config");
        let json = with_config
            .to_config_json(dir.path())
            .expect("to_config_json");
        assert_eq!(json["config"]["address"], "10.0.0.1");
        assert_eq!(json["config"]["channel"], 2);

        // a config-less plug emits a null config, which the runtime treats as {}
        let without_config: PlugDefinition =
            crate::procedure::yaml::from_str("name: Rigol\npython: rigol:Rigol\n")
                .expect("parse plug without config");
        let json = without_config
            .to_config_json(dir.path())
            .expect("to_config_json");
        assert!(json["config"].is_null());
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), 30_000);
        assert_eq!(parse_duration("1s").unwrap(), 1_000);
        assert_eq!(parse_duration("90s").unwrap(), 90_000);
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), 300_000);
        assert_eq!(parse_duration("1m").unwrap(), 60_000);
        assert_eq!(parse_duration("10m").unwrap(), 600_000);
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), 3_600_000);
        assert_eq!(parse_duration("2h").unwrap(), 7_200_000);
    }

    #[test]
    fn test_parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), 500);
        assert_eq!(parse_duration("1000ms").unwrap(), 1_000);
        assert_eq!(parse_duration("100ms").unwrap(), 100);
    }

    #[test]
    fn test_parse_duration_combined() {
        assert_eq!(parse_duration("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration("2h15m30s").unwrap(), 8_130_000);
        assert_eq!(parse_duration("1m30s").unwrap(), 90_000);
        assert_eq!(parse_duration("1h500ms").unwrap(), 3_600_500);
    }

    #[test]
    fn test_parse_duration_with_whitespace() {
        assert_eq!(parse_duration("  30s  ").unwrap(), 30_000);
        assert_eq!(parse_duration("5m ").unwrap(), 300_000);
    }

    #[test]
    fn test_parse_duration_invalid_no_unit() {
        let result = parse_duration("30");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing unit"));
    }

    #[test]
    fn test_parse_duration_invalid_zero() {
        let result = parse_duration("0s");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "invalid duration `0s`: expected > 0");
    }

    #[test]
    fn a_spelled_out_unit_is_named_whole_with_its_short_form() {
        assert_eq!(
            parse_duration("30 sec").unwrap_err(),
            "invalid duration `30 sec`: unknown unit `sec`; write `30s`"
        );
        assert_eq!(
            parse_duration("2 hours").unwrap_err(),
            "invalid duration `2 hours`: unknown unit `hours`; write `2h`"
        );
        assert_eq!(parse_duration("1h 30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration("250ms").unwrap(), 250);
    }

    #[test]
    fn test_parse_duration_invalid_character() {
        let result = parse_duration("30x");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown unit `x`"));
    }

    #[test]
    fn test_parse_duration_empty() {
        let result = parse_duration("");
        assert!(result.is_err());
    }

    #[test]
    fn test_format_duration_milliseconds() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(999), "999ms");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(30_000), "30s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60_000), "1m");
        assert_eq!(format_duration(300_000), "5m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(7_200_000), "2h");
    }

    #[test]
    fn test_format_duration_combined() {
        assert_eq!(format_duration(5_400_000), "1h30m");
        assert_eq!(format_duration(90_000), "1m30s");
        assert_eq!(format_duration(3_661_000), "1h1m1s");
        assert_eq!(format_duration(3_600_500), "1h500ms");
    }

    #[test]
    fn test_duration_roundtrip() {
        let inputs = vec!["30s", "5m", "1h", "1h30m", "500ms", "2h15m30s"];

        for input in inputs {
            let parsed = parse_duration(input).unwrap();
            let formatted = format_duration(parsed);
            let reparsed = parse_duration(&formatted).unwrap();
            assert_eq!(parsed, reparsed, "Roundtrip failed for input: {}", input);
        }
    }

    #[test]
    fn test_validate_python_identifier_valid() {
        assert!(validate_python_identifier("valid_name").is_ok());
        assert!(validate_python_identifier("test123").is_ok());
        assert!(validate_python_identifier("_private").is_ok());
        assert!(validate_python_identifier("CamelCase").is_ok());
    }

    #[test]
    fn test_validate_python_identifier_invalid() {
        assert!(validate_python_identifier("123invalid").is_err());
        assert!(validate_python_identifier("invalid-name").is_err());
        assert!(validate_python_identifier("invalid name").is_err());
        assert!(validate_python_identifier("").is_err());
    }

    // `slots:` integer shorthand
    #[test]
    fn slots_integer_shorthand_expands_padded_ordinals() {
        let cfg: ExecutionConfig = crate::procedure::yaml::from_str("slots: 16").unwrap();
        assert_eq!(cfg.slots.len(), 16);
        assert_eq!(cfg.slots[0].key, "slot_01");
        assert_eq!(cfg.slots[0].name, "Slot 1");
        assert_eq!(cfg.slots[15].key, "slot_16");

        let cfg300: ExecutionConfig = crate::procedure::yaml::from_str("slots: 300").unwrap();
        assert_eq!(cfg300.slots[0].key, "slot_001");
        assert_eq!(cfg300.slots[299].key, "slot_300");
    }

    #[test]
    fn slots_explicit_list_still_parses() {
        let cfg: ExecutionConfig =
            crate::procedure::yaml::from_str("slots:\n  - key: nest_a\n    name: Left Nest")
                .unwrap();
        assert_eq!(cfg.slots.len(), 1);
        assert_eq!(cfg.slots[0].key, "nest_a");
    }

    #[test]
    fn slots_shorthand_rejects_bad_counts_with_a_reason() {
        let zero = crate::procedure::yaml::from_str::<ExecutionConfig>("slots: 0").unwrap_err();
        assert!(zero.to_string().contains("at least 1"), "{zero}");
        let neg = crate::procedure::yaml::from_str::<ExecutionConfig>("slots: -1").unwrap_err();
        assert!(neg.to_string().contains("at least 1"), "{neg}");
        let huge =
            crate::procedure::yaml::from_str::<ExecutionConfig>("slots: 100000").unwrap_err();
        assert!(huge.to_string().contains("expected at most"), "{huge}");
        let text =
            crate::procedure::yaml::from_str::<ExecutionConfig>("slots: \"80\"").unwrap_err();
        assert!(
            text.to_string()
                .contains("expected a slot count or a list of slots"),
            "{text}"
        );
    }

    #[test]
    fn slots_list_entry_errors_keep_their_own_message() {
        let err = crate::procedure::yaml::from_str::<ExecutionConfig>("slots:\n  - key: nest_a")
            .unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    fn locked_field(value: &str) -> UnitFieldConfig {
        UnitFieldConfig {
            default_value: Some(value.to_string()),
            locked: true,
            ..UnitFieldConfig::default()
        }
    }

    /// TP-1093: a lock with nothing in it cannot identify the unit, so
    /// it is refused where `auto_identify` without defaults is refused.
    #[test]
    fn locked_requires_a_non_blank_default() {
        let cfg = UnitConfig {
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig {
                locked: true,
                ..UnitFieldConfig::default()
            }),
            ..UnitConfig::default()
        };
        let err = cfg.validate_defaults().unwrap_err();
        assert_eq!(err, "unit.part_number: `locked` needs `default_value`");

        let blank = UnitConfig {
            batch_number: Some(locked_field("   ")),
            ..UnitConfig::default()
        };
        let err = blank.validate_defaults().unwrap_err();
        assert!(
            err.starts_with("unit.batch_number: `locked` needs"),
            "{err}"
        );

        // Sub-units and metadata name their own path.
        let sub = UnitConfig {
            sub_units: Some(SubUnitsConfig(vec![SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: Some(UnitFieldConfig {
                    locked: true,
                    ..UnitFieldConfig::default()
                }),
            }])),
            ..UnitConfig::default()
        };
        let err = sub.validate_defaults().unwrap_err();
        assert!(
            err.starts_with("unit.sub_units[0].serial_number: `locked`"),
            "{err}"
        );

        // The root operated_by goes through the same free function.
        let err = validate_field_default(
            "operated_by",
            &UnitFieldConfig {
                locked: true,
                ..UnitFieldConfig::default()
            },
        )
        .unwrap_err();
        assert_eq!(err, "operated_by: `locked` needs `default_value`");
    }

    /// A locked default is judged by its own rules at load: the typo in
    /// a fixed part number fails the deploy, not the first run.
    #[test]
    fn locked_default_must_pass_its_own_pattern_and_lengths() {
        let bad_pattern = UnitFieldConfig {
            pattern: Some("^PCB-[A-Z0-9]+$".to_string()),
            ..locked_field("pcb main")
        };
        let err = validate_field_default("unit.part_number", &bad_pattern).unwrap_err();
        assert!(err.starts_with("unit.part_number.default_value: "), "{err}");
        assert!(!err.contains("^PCB-"), "raw regex must not leak: {err}");

        let too_long = UnitFieldConfig {
            max_length: Some(3),
            ..locked_field("PCB-MAIN")
        };
        assert!(validate_field_default("unit.part_number", &too_long).is_err());

        let fine = UnitFieldConfig {
            pattern: Some("^PCB-[A-Z0-9-]+$".to_string()),
            ..locked_field("PCB-MAIN-V2")
        };
        validate_field_default("unit.part_number", &fine).unwrap();
    }

    /// `{slot}` is expanded per slot at identify time, where the expanded
    /// value is validated; at load only presence can be judged.
    #[test]
    fn locked_default_with_slot_placeholder_is_only_checked_for_presence() {
        let cfg = UnitFieldConfig {
            pattern: Some("^BURNIN-slot_[0-9]{2}$".to_string()),
            ..locked_field("BURNIN-{slot}")
        };
        validate_field_default("unit.serial_number", &cfg).unwrap();
    }

    /// An unlocked default is judged the same way: it pre-fills a form
    /// the operator would have to correct on every run, and Studio
    /// reports it as an issue that keeps the run from starting.
    /// Metadata keys included — that is where it was first missed.
    #[test]
    fn unlocked_default_is_judged_at_load_too() {
        let cfg = UnitFieldConfig {
            pattern: Some("^PCB-".to_string()),
            default_value: Some("nope".to_string()),
            ..UnitFieldConfig::default()
        };
        let err = validate_field_default("unit.part_number", &cfg).unwrap_err();
        assert!(err.starts_with("unit.part_number.default_value: "), "{err}");

        let mut md = std::collections::BTreeMap::new();
        md.insert(
            "color".to_string(),
            UnitFieldConfig {
                pattern: Some("^(red|blue)$".to_string()),
                default_value: Some("green".to_string()),
                ..UnitFieldConfig::default()
            },
        );
        let unit = UnitConfig {
            metadata: Some(md),
            ..UnitConfig::default()
        };
        let err = unit.validate_defaults().unwrap_err();
        assert!(
            err.starts_with("unit.metadata.color.default_value: "),
            "{err}"
        );

        // A blank default on an unlocked field is "no default", not an
        // entry to judge; a matching one passes.
        let blank = UnitFieldConfig {
            pattern: Some("^PCB-".to_string()),
            default_value: Some("  ".to_string()),
            ..UnitFieldConfig::default()
        };
        validate_field_default("unit.part_number", &blank).unwrap();
        let fine = UnitFieldConfig {
            pattern: Some("^PCB-[A-Z]+$".to_string()),
            default_value: Some("PCB-MAIN".to_string()),
            ..UnitFieldConfig::default()
        };
        validate_field_default("unit.part_number", &fine).unwrap();
    }

    /// Mirrors `build_components`: every component the form would show
    /// counts, and only when all of them are locked does the form go.
    #[test]
    fn prompts_operator_is_false_only_when_every_shown_field_is_locked() {
        let both = UnitConfig {
            serial_number: Some(locked_field("SN-1")),
            part_number: Some(locked_field("PCB")),
            ..UnitConfig::default()
        };
        assert!(!both.prompts_operator(None));

        // Serial and part are always shown: undeclared still prompts.
        assert!(UnitConfig::default().prompts_operator(None));
        let serial_only = UnitConfig {
            serial_number: Some(locked_field("SN-1")),
            ..UnitConfig::default()
        };
        assert!(serial_only.prompts_operator(None));

        // Declared optional fields count; locked ones do not.
        let mut with_batch = both.clone();
        with_batch.batch_number = Some(UnitFieldConfig::default());
        assert!(with_batch.prompts_operator(None));
        with_batch.batch_number = Some(locked_field("LOT-1"));
        assert!(!with_batch.prompts_operator(None));

        // The root operated_by is prompted on the same screen.
        assert!(both.prompts_operator(Some(&UnitFieldConfig::default())));
        assert!(!both.prompts_operator(Some(&locked_field("op@acme.com"))));

        // Sub-units and metadata too.
        let mut with_sub = both.clone();
        with_sub.sub_units = Some(SubUnitsConfig(vec![SubUnitItemConfig {
            label: "Battery".to_string(),
            key: None,
            serial_number: None,
        }]));
        assert!(with_sub.prompts_operator(None));
        with_sub.sub_units = Some(SubUnitsConfig(vec![SubUnitItemConfig {
            label: "Battery".to_string(),
            key: None,
            serial_number: Some(locked_field("BAT-1")),
        }]));
        assert!(!with_sub.prompts_operator(None));

        let mut with_md = both.clone();
        let mut md = std::collections::BTreeMap::new();
        md.insert("line".to_string(), UnitFieldConfig::default());
        with_md.metadata = Some(md.clone());
        assert!(with_md.prompts_operator(None));
        md.insert("line".to_string(), locked_field("L2"));
        with_md.metadata = Some(md);
        assert!(!with_md.prompts_operator(None));
    }

    /// `locked: false` is the default and stays out of the file.
    #[test]
    fn locked_round_trips_and_defaults_to_false() {
        let cfg: UnitFieldConfig =
            crate::procedure::yaml::from_str("default_value: PCB\nlocked: true").unwrap();
        assert!(cfg.locked);
        let plain: UnitFieldConfig =
            crate::procedure::yaml::from_str("default_value: PCB").unwrap();
        assert!(!plain.locked);
        let out = crate::procedure::yaml::to_string(&plain);
        assert!(!out.contains("locked"), "{out}");
        let out = crate::procedure::yaml::to_string(&cfg);
        assert!(out.contains("locked: true"), "{out}");
    }
}

#[cfg(all(test, feature = "json-schema"))]
mod json_schema_tests {
    use super::*;

    /// Every property of every object in the schema must carry a
    /// description: the assistant reads nothing else about a field.
    fn undocumented(node: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        if let Some(props) = node.get("properties").and_then(|p| p.as_object()) {
            for (name, prop) in props {
                let here = format!("{path}.{name}");
                if prop.get("description").is_none() {
                    out.push(here.clone());
                }
                undocumented(prop, &here, out);
            }
        }
        for key in ["items", "additionalProperties"] {
            if let Some(child) = node.get(key) {
                undocumented(child, &format!("{path}[{key}]"), out);
            }
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(list) = node.get(key).and_then(|l| l.as_array()) {
                for (i, child) in list.iter().enumerate() {
                    undocumented(child, &format!("{path}[{key}][{i}]"), out);
                }
            }
        }
    }

    #[test]
    fn every_field_has_a_description() {
        let schema = procedure_json_schema();
        let mut missing = Vec::new();
        undocumented(&schema, "procedure", &mut missing);
        if let Some(defs) = schema.get("$defs").and_then(|d| d.as_object()) {
            for (name, def) in defs {
                undocumented(def, name, &mut missing);
            }
        }
        assert!(
            missing.is_empty(),
            "fields without a /// doc comment: {missing:?}"
        );
    }

    #[test]
    fn the_phase_lists_its_flow_fields() {
        let schema = procedure_json_schema();
        let phase = &schema["$defs"]["PhaseDefinitionYaml"]["properties"];
        for field in [
            "then",
            "retry",
            "depends_on",
            "timeout",
            "enabled",
            "executable",
        ] {
            assert!(
                phase.get(field).is_some(),
                "phase.{field} missing from the schema"
            );
        }
        // schemars drops `default` on a `skip_serializing_if` field: the doc
        // comment carries it instead.
        assert!(phase["enabled"]["description"]
            .as_str()
            .unwrap_or("")
            .contains("Default true"));
        let then = schema["$defs"]["ThenConfig"]["properties"]["fail"].to_string();
        assert!(then.contains("#/$defs/PhaseNextAction"), "{then}");
        assert_eq!(
            schema["$defs"]["PhaseNextAction"]["enum"],
            serde_json::json!(["continue", "stop", "retry"])
        );
    }

    #[test]
    fn durations_and_slots_are_described_as_written() {
        let schema = procedure_json_schema();
        let timeout = schema["$defs"]["PhaseDefinitionYaml"]["properties"]["timeout"].to_string();
        assert!(timeout.contains("Duration"), "{timeout}");
        assert!(schema["$defs"]["Duration"]["pattern"].is_string());
        let slots = schema["$defs"]["ExecutionConfig"]["properties"]["slots"].to_string();
        assert!(slots.contains("SlotsSchema"), "{slots}");
    }
}
