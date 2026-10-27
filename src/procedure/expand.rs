//! Expansion of sequence calls: from a procedure YAML whose lists hold
//! `uses:` entries to the flat `ProcedureDefinition` the orchestrator runs.
//!
//! Loading stays structural (`inspect_procedure_definition` parses a
//! string); this step is the one that reads other files. For every call
//! it locates the sequence file (relative to the file holding the call,
//! or inside a Python package), refuses cycles and over-deep nesting,
//! resolves the call's `with:` against the file's `inputs:` and
//! substitutes the `{inputs.<name>}` markers in the file's YAML, binds the
//! file's plug requirements to the procedure's plugs, then expands the
//! file's phases with:
//!
//! - keys prefixed by the call's instance key (`calib/zero`), so two calls
//!   of one file never collide and the orchestrator addresses them as it
//!   addresses any phase;
//! - `python:` and `working_directory` rewritten into the procedure
//!   directory's frame, since the worker resolves them from there;
//! - the sequence's own barriers as implicit `depends_on`: its main waits
//!   for its setup, its teardown for its setup and main;
//! - `sequence_path` / `sequence_stages` / `inputs` / `plug_aliases` /
//!   `source` filled, which is what the orchestrator, the worker and the
//!   diagnostics read.
//!
//! The tree of calls is kept on `ProcedureDefinition::sequences`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use super::loader::LoadError;
use super::schema::{
    PhaseDefinition, PhaseEntryYaml, PhaseStage, ProcedureDefinition, ProcedureYaml, PythonSpec,
    RefList, RefLocation, Scope, SequenceCallYaml,
};
use super::sequence::{InputSpecYaml, PhaseSource, SequenceInstance, SequenceYaml};

/// Deepest allowed nesting of sequence calls (a call made in the procedure
/// is depth 1). A cap, not a design limit: it stops a runaway include
/// graph before the job queue does.
pub const MAX_SEQUENCE_DEPTH: usize = 8;

/// Finds the file a `uses:` reference names. The engine ships a
/// filesystem locator; the CLI wraps it with the procedure's Python
/// environment so `<package>:<path>` references resolve too.
pub trait SequenceLocator {
    /// `reference` as written on the call; `base_dir` is the directory of
    /// the file holding the call. The error is shown to the author.
    fn locate(&self, reference: &str, base_dir: &Path) -> Result<PathBuf, String>;

    /// False when a `<package>:<path>` reference cannot be resolved here
    /// at all (no interpreter): its refusal is "not checked", not a broken
    /// procedure.
    fn resolves_packages(&self) -> bool {
        true
    }
}

/// Relative paths only. A package reference is an error naming what is
/// missing (the Python environment), so the message says what to do
/// rather than "file not found".
pub struct FsSequenceLocator;

impl SequenceLocator for FsSequenceLocator {
    fn locate(&self, reference: &str, base_dir: &Path) -> Result<PathBuf, String> {
        if let Some((package, _)) = split_package_reference(reference) {
            return Err(format!(
                "`{reference}` names a file of Python package `{package}`, which needs the procedure's Python environment to be found: run from the procedure directory with its environment, or write a path relative to this file"
            ));
        }
        resolve_file_reference(reference, base_dir)
    }

    fn resolves_packages(&self) -> bool {
        false
    }
}

/// Resolves `<package>:<path>` through a Python interpreter (the
/// procedure's venv): `importlib.resources.files(package) / path`. Falls
/// back to the filesystem locator for plain paths. One subprocess per
/// distinct reference, cached for the locator's lifetime.
pub struct PythonPackageLocator {
    python: PathBuf,
    cache: std::cell::RefCell<HashMap<String, Result<PathBuf, String>>>,
}

impl PythonPackageLocator {
    pub fn new(python: impl Into<PathBuf>) -> Self {
        Self {
            python: python.into(),
            cache: Default::default(),
        }
    }
}

impl SequenceLocator for PythonPackageLocator {
    fn locate(&self, reference: &str, base_dir: &Path) -> Result<PathBuf, String> {
        let Some((package, inner)) = split_package_reference(reference) else {
            return resolve_file_reference(reference, base_dir);
        };
        if let Some(cached) = self.cache.borrow().get(reference) {
            return cached.clone();
        }
        let result = locate_in_package(&self.python, package, inner, reference);
        self.cache
            .borrow_mut()
            .insert(reference.to_string(), result.clone());
        result
    }
}

fn locate_in_package(
    python: &Path,
    package: &str,
    inner: &str,
    reference: &str,
) -> Result<PathBuf, String> {
    const SCRIPT: &str = "import importlib.resources, sys\nprint(importlib.resources.files(sys.argv[1]).joinpath(sys.argv[2]))";
    // `files()` imports the package, so its `__init__.py` runs: user code,
    // kept from the run's credentials like every other spawn
    // (`procedure_env`).
    let mut command = std::process::Command::new(python);
    command
        .arg("-I")
        .arg("-c")
        .arg(SCRIPT)
        .arg(package)
        .arg(inner);
    crate::procedure_env::strip_inherited_std(&mut command);
    let output = command.output().map_err(|e| {
        format!(
            "`{reference}`: cannot run {} to find package `{package}`: {e}",
            python.display()
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().last().unwrap_or("").trim();
        return Err(format!(
            "`{reference}`: package `{package}` is not installed in the procedure's environment ({last}). Add it to pyproject.toml, or write a path relative to this file"
        ));
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !path.is_file() {
        return Err(format!(
            "`{reference}`: package `{package}` is installed but holds no file `{inner}` (looked at {})",
            path.display()
        ));
    }
    Ok(path)
}

/// The call's `with.<name>` line, where the entry is known: the input
/// an error is about, not the call's `uses:`.
fn at_with(location: &Option<RefLocation>, name: &str) -> Option<RefLocation> {
    location.clone().map(|l| RefLocation {
        key: Some(format!("with.{name}")),
        ..l
    })
}

fn located(message: impl Into<String>, location: Option<RefLocation>) -> LoadError {
    match location {
        Some(at) => LoadError::at(message, at),
        None => LoadError::text(message),
    }
}

/// `<package>:<path>` when `reference` has that shape: the part before the
/// first `:` is a dotted Python package name (no path separator, not a
/// one-letter Windows drive), the part after is a relative path.
pub fn split_package_reference(reference: &str) -> Option<(&str, &str)> {
    let (package, inner) = reference.split_once(':')?;
    let package = package.trim();
    let inner = inner.trim();
    if package.is_empty() || inner.is_empty() || package.len() == 1 {
        return None;
    }
    if !package
        .split('.')
        .all(crate::python::is_valid_python_identifier)
    {
        return None;
    }
    if inner.starts_with('/') || inner.starts_with('\\') {
        return None;
    }
    Some((package, inner))
}

/// A plain file reference, relative to `base_dir`. Absolute paths and `~`
/// are refused: they do not travel with a deployment.
pub fn resolve_file_reference(reference: &str, base_dir: &Path) -> Result<PathBuf, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("`uses:` is empty: name the sequence file".to_string());
    }
    let as_path = Path::new(reference);
    let windows_drive = reference.as_bytes().get(1).is_some_and(|b| *b == b':')
        && reference
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic());
    if as_path.is_absolute()
        || reference.starts_with('/')
        || reference.starts_with('~')
        || windows_drive
    {
        return Err(format!(
            "`{reference}`: absolute paths and `~` do not travel with a deployment; write the path relative to this file"
        ));
    }
    let ext = as_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("yaml") | Some("yml")) {
        return Err(format!(
            "`{reference}`: a sequence file ends in .yaml or .yml"
        ));
    }
    let joined = normalize_lexically(&base_dir.join(as_path));
    if !joined.is_file() {
        return Err(format!(
            "sequence file not found: {} (`{reference}`, relative to {})",
            joined.display(),
            base_dir.display()
        ));
    }
    Ok(joined)
}

/// Collapse `.` and `..` without touching the disk, so a missing file is
/// reported at the path the author meant.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `to` relative to `from`, both directories, as a `/`-joined string the
/// worker's path resolution understands. Empty when equal. Compares the
/// canonical paths when both exist, so symlinked checkouts agree.
pub fn relative_dir(from: &Path, to: &Path) -> String {
    // Both canonical or both lexical: mixing the two compares a resolved
    // symlink (macOS's /private/var) with an unresolved one.
    let (from, to) = match (from.canonicalize(), to.canonicalize()) {
        (Ok(f), Ok(t)) => (f, t),
        _ => (normalize_lexically(from), normalize_lexically(to)),
    };
    let from_parts: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut segments: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        segments.push("..".to_string());
    }
    for part in &to_parts[common..] {
        segments.push(part.as_os_str().to_string_lossy().to_string());
    }
    segments.join("/")
}

/// The procedure, expanded. `Err` holds every problem found, so the
/// author fixes them in one pass; `Ok` holds a definition the loader's
/// post-parse rules then judge like any other.
pub fn expand_procedure(
    raw: ProcedureYaml,
    procedure_dir: &Path,
    locator: &dyn SequenceLocator,
) -> Result<ProcedureDefinition, Vec<LoadError>> {
    expand_procedure_rooted(raw, procedure_dir, procedure_dir, locator)
}

/// `expand_procedure` for a procedure whose `python:` references resolve
/// against another directory than the one holding its file: a deployment
/// whose manifest `entry_point` names a YAML in a subdirectory resolves
/// them against the package directory. `uses:` references still resolve
/// from the file; only the rebase of the sequence phases' Python specs
/// uses `python_root`, so they land where the worker will look.
pub fn expand_procedure_rooted(
    raw: ProcedureYaml,
    procedure_dir: &Path,
    python_root: &Path,
    locator: &dyn SequenceLocator,
) -> Result<ProcedureDefinition, Vec<LoadError>> {
    let procedure_plugs: Vec<String> = raw
        .plugs
        .iter()
        .map(|p| {
            p.key
                .clone()
                .unwrap_or_else(|| crate::python::to_python_identifier(&p.name))
        })
        .collect();

    let mut ctx = Ctx {
        procedure_dir,
        python_root,
        locator,
        errors: Vec::new(),
        instances: Vec::new(),
        entry_instances: HashMap::new(),
        stack: Vec::new(),
    };

    let frame = Frame {
        path: Vec::new(),
        stages: Vec::new(),
        base_dir: procedure_dir.to_path_buf(),
        file: None,
        rel_dir: String::new(),
        inputs: BTreeMap::new(),
        plug_aliases: BTreeMap::new(),
        available_plugs: procedure_plugs,
        scope: None,
        top_level: true,
        location: None,
        value_scope: None,
    };

    let ProcedureYaml {
        id,
        name,
        version,
        description,
        execution,
        unit,
        operated_by,
        ui,
        config,
        secrets,
        env,
        plugs,
        setup,
        main,
        teardown,
    } = raw;

    let mut setup_phases = ctx.expand_list(&setup, &frame, PhaseStage::Setup, None);
    let mut main_phases = ctx.expand_list(&main, &frame, PhaseStage::Main, None);
    let mut teardown_phases = ctx.expand_list(&teardown, &frame, PhaseStage::Teardown, None);
    ctx.check_level_keys(
        &[&setup_phases, &main_phases, &teardown_phases],
        &frame,
        &[
            (&setup, PhaseStage::Setup),
            (&main, PhaseStage::Main),
            (&teardown, PhaseStage::Teardown),
        ],
    );
    ctx.resolve_call_dependencies(&mut setup_phases, &frame);
    ctx.resolve_call_dependencies(&mut main_phases, &frame);
    ctx.resolve_call_dependencies(&mut teardown_phases, &frame);

    if !ctx.errors.is_empty() {
        return Err(ctx.errors);
    }

    Ok(ProcedureDefinition {
        id,
        name,
        version,
        description,
        execution,
        unit,
        operated_by,
        ui,
        config,
        secrets,
        env,
        plugs: plugs.into_iter().map(|p| p.into()).collect(),
        setup: setup_phases,
        main: main_phases,
        teardown: teardown_phases,
        sequences: ctx.instances,
    })
}

struct Ctx<'a> {
    procedure_dir: &'a Path,
    /// What the sequence phases' `python:` specs are rebased against: the
    /// directory the worker resolves them from (the procedure directory,
    /// or the package directory for a nested entry point).
    python_root: &'a Path,
    locator: &'a dyn SequenceLocator,
    errors: Vec<LoadError>,
    instances: Vec<SequenceInstance>,
    /// (frame path, list, index of the call entry) to its instance's
    /// position in `instances`, for the per-level key check.
    entry_instances: HashMap<(Vec<String>, PhaseStage, usize), usize>,
    /// Canonical paths of the sequence files being expanded, outermost
    /// first: a reference to one of them is a cycle.
    stack: Vec<(PathBuf, String)>,
}

/// What the entries of one list inherit from the file and call they sit
/// in.
#[derive(Clone)]
struct Frame {
    /// Instance keys down to the sequence whose list is being expanded;
    /// empty for the procedure's own lists.
    path: Vec<String>,
    /// For each ancestor sequence, the stage its child (the next call or
    /// the phase) sits in. One shorter than `path` plus the current
    /// stage, which `expand_list` appends.
    stages: Vec<PhaseStage>,
    /// Directory of the file whose list is being expanded.
    base_dir: PathBuf,
    /// That file, `None` for the procedure.
    file: Option<PathBuf>,
    /// `base_dir` relative to the procedure directory, `/`-joined; empty
    /// for the procedure.
    rel_dir: String,
    inputs: BTreeMap<String, serde_json::Value>,
    /// Plug parameter name in this file to procedure plug key.
    plug_aliases: BTreeMap<String, String>,
    /// Plug keys a call in this file may bind to: the procedure's plug
    /// keys at the top, the enclosing sequence's requirement keys below.
    available_plugs: Vec<String>,
    /// Scope every phase of this frame takes (set by a call made from the
    /// procedure's setup or teardown).
    scope: Option<Scope>,
    top_level: bool,
    /// The top-level call this frame descends from, for diagnostics.
    location: Option<RefLocation>,
    /// The config and secrets the file of this frame declares; `None` for
    /// the procedure, whose phases read its own declarations.
    value_scope: Option<super::values::ValueScope>,
}

impl Frame {
    /// `<file>: ` in front of a diagnostic about this frame's file, nothing
    /// for the procedure's own lists.
    fn in_file(&self) -> String {
        self.file
            .as_ref()
            .map(|f| format!("{}: ", f.display()))
            .unwrap_or_default()
    }
}

fn ref_list(stage: PhaseStage) -> RefList {
    match stage {
        PhaseStage::Setup => RefList::Setup,
        PhaseStage::Main => RefList::Main,
        PhaseStage::Teardown => RefList::Teardown,
    }
}

impl<'a> Ctx<'a> {
    fn error(&mut self, message: impl Into<String>, location: Option<RefLocation>) {
        self.errors.push(located(message, location));
    }

    /// An error whose message names its position in the sequence file:
    /// still reported at the call, without a line of the procedure file.
    fn error_in_file(&mut self, message: impl Into<String>, location: Option<RefLocation>) {
        self.errors.push(LoadError::at_call(message, location));
    }

    /// One list of a file, as flat phases. Sequence calls contribute all
    /// their phases in place, setup first, then main, then teardown.
    fn expand_list(
        &mut self,
        entries: &[PhaseEntryYaml],
        frame: &Frame,
        stage: PhaseStage,
        // The same list as the sequence file writes it, markers in place
        // (see `written_lists`). `None` for the procedure's own lists, which
        // have no substituted form. An entry with a marker in a typed field
        // (`limit: "{inputs.n}"`) has no written form of its own (`None` in
        // the list), its siblings keep theirs.
        written: Option<&[Option<PhaseEntryYaml>]>,
    ) -> Vec<PhaseDefinition> {
        let mut out = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let written_entry = written.and_then(|w| w.get(index)).and_then(|e| e.clone());
            match entry {
                PhaseEntryYaml::Phase(yaml) => {
                    let written_phase = match written_entry {
                        Some(PhaseEntryYaml::Phase(y)) => Some(*y),
                        _ => None,
                    };
                    out.push(self.expand_phase(
                        (**yaml).clone(),
                        frame,
                        stage,
                        index,
                        written_phase,
                    ));
                }
                PhaseEntryYaml::Uses(call) => {
                    let written_call = match &written_entry {
                        Some(PhaseEntryYaml::Uses(w)) => Some(w),
                        _ => None,
                    };
                    out.extend(self.expand_call(call, written_call, frame, stage, index));
                }
            }
        }
        out
    }

    fn expand_phase(
        &mut self,
        yaml: super::schema::PhaseDefinitionYaml,
        frame: &Frame,
        stage: PhaseStage,
        index: usize,
        written: Option<super::schema::PhaseDefinitionYaml>,
    ) -> PhaseDefinition {
        // `dashboard` is the parameter the worker fills in with the API
        // client, before plug keys and phase results: a phase keyed so is
        // unreachable from every phase. The loader refuses it in the
        // procedure, on the expanded lists, where a sequence phase's key
        // carries the call's prefix and never matches: the file's own key
        // is checked here, at the call.
        if !frame.top_level {
            let local = yaml
                .key
                .clone()
                .unwrap_or_else(|| crate::python::to_python_identifier(&yaml.name));
            if local.eq_ignore_ascii_case("dashboard") {
                let in_file = frame.in_file();
                self.error(
                    format!(
                        "{in_file}phase `{}` has key `dashboard`, which is the parameter a phase receives the TofuPilot API client through: a later phase naming it would get the client, never this phase's results. Set another `key:`",
                        yaml.name
                    ),
                    frame.location.clone(),
                );
            }
        }
        // The expanded key is a `/` path, the written one is not: it is the
        // identifier phases and plugs always had to be, 1 to 100 characters.
        if let Some(written) = yaml.key.as_deref() {
            let len = written.chars().count();
            if !(1..=100).contains(&len) || !crate::python::is_valid_python_identifier(written) {
                let in_file = frame.in_file();
                let location = if frame.top_level {
                    Some(RefLocation::key(ref_list(stage), index, "key"))
                } else {
                    frame.location.clone()
                };
                self.error(
                    format!(
                        "{in_file}phase `{}`: key `{written}` must be a Python identifier of 1 to 100 characters",
                        yaml.name
                    ),
                    location,
                );
            }
        }
        // The entry as written, kept beside the expanded phase for the
        // editors of the sequence file (Studio): what they show and write
        // back must be the file's own form, not the substituted, prefixed
        // and barrier-laden one that runs.
        let written = written
            .filter(|_| !frame.top_level)
            .map(|y| Box::new(PhaseDefinition::from(y)));
        let mut phase = PhaseDefinition::from(yaml);
        if frame.top_level {
            return phase;
        }
        phase.written = written;
        let file = frame
            .file
            .as_ref()
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        if phase.scope.is_some() {
            self.error(
                format!(
                    "{}: phase `{}` has `scope:`, which is not allowed in a sequence file; every phase of the sequence takes the call's scope",
                    file, phase.key
                ),
                frame.location.clone(),
            );
        }
        phase.scope = frame.scope;
        let prefix = frame.path.join("/");
        phase.key = format!("{prefix}/{}", phase.key);
        phase.depends_on = phase
            .depends_on
            .iter()
            .map(|d| format!("{prefix}/{d}"))
            .collect();
        phase.sequence_path = frame.path.clone();
        phase.sequence_stages = frame.stages.iter().copied().chain([stage]).collect();
        phase.inputs = frame.inputs.clone();
        phase.plug_aliases = frame.plug_aliases.clone();
        phase.value_scope = frame.value_scope.clone();
        phase.source = Some(PhaseSource {
            file,
            list: stage,
            index,
        });
        if let Some(spec) = &phase.python {
            // A dotted spec names an installed module when nothing in the
            // sequence file's directory could be it: no file for the
            // module and no folder or module for its first segment. It
            // then stays as written, so the worker's import fallback
            // applies as it does for a phase of the procedure. A missing
            // file under a folder that exists is a typo, and keeps the
            // rebased path the ref check reports as "file not found".
            let rebased = rebase_python_spec(spec, &frame.rel_dir);
            phase.python =
                if names_installed_module(spec, &rebased, &frame.base_dir, self.python_root) {
                    Some(spec.clone())
                } else {
                    Some(rebased)
                };
        }
        if let Some(exec) = &mut phase.executable {
            if !frame.rel_dir.is_empty() {
                match &exec.working_directory {
                    Some(wd) if !Path::new(wd).is_absolute() => {
                        exec.working_directory = Some(format!("{}/{}", frame.rel_dir, wd));
                    }
                    Some(_) => {}
                    None => exec.working_directory = Some(frame.rel_dir.clone()),
                }
            }
        }
        phase
    }

    fn expand_call(
        &mut self,
        call: &SequenceCallYaml,
        // The entry as the enclosing sequence file writes it, markers in
        // place, for a nested call whose entry types unsubstituted. The
        // instance keeps the written `name:`, `with:`, `plugs:`,
        // `depends_on` and `description:` for the editor of that file,
        // which must show and write the file's own form (a `with:` passing
        // `{inputs.ch}` down, not the value of one caller). `None` for a
        // call written in the procedure, whose entry is its own written
        // form, and for an entry with a marker in a typed field, which
        // falls back to the substituted entry.
        written: Option<&SequenceCallYaml>,
        frame: &Frame,
        stage: PhaseStage,
        index: usize,
    ) -> Vec<PhaseDefinition> {
        let as_written = written.unwrap_or(call);
        // Where diagnostics about this call point: the entry itself when it
        // is in the procedure, the top-level call it descends from below.
        let location = if frame.top_level {
            Some(RefLocation::entry(ref_list(stage), index))
        } else {
            frame.location.clone()
        };
        let in_file = frame.in_file();
        let label = call
            .name
            .clone()
            .or_else(|| call.key.clone())
            .unwrap_or_else(|| call.uses.clone());

        if call.scope.is_some() && !(frame.top_level && stage != PhaseStage::Main) {
            self.error(
                format!(
                    "{in_file}sequence call `{label}` has `scope:`, which only a call made from the procedure's setup or teardown may set; in main a call always runs per slot, and inside a sequence it takes the enclosing call's scope"
                ),
                location.clone(),
            );
        }

        // `station` is a plug scope. Refused here, at the call, rather than
        // by the loader on an expanded phase the author never wrote.
        if call.scope == Some(crate::procedure::schema::Scope::Station) {
            self.error(
                format!(
                    "{in_file}sequence call `{label}`: `scope: station` is for plugs only, a call takes `slot` or `execution`"
                ),
                location.clone(),
            );
        }

        if frame.path.len() + 1 > MAX_SEQUENCE_DEPTH {
            self.error(
                format!(
                    "{in_file}sequence call `{label}` nests deeper than {MAX_SEQUENCE_DEPTH} levels; flatten the calls"
                ),
                location,
            );
            return Vec::new();
        }

        let file = match self.locator.locate(&call.uses, &frame.base_dir) {
            Ok(f) => f,
            Err(e) => {
                let message = format!("{in_file}sequence call `{label}`: {e}");
                if split_package_reference(&call.uses).is_some()
                    && !self.locator.resolves_packages()
                {
                    self.errors
                        .push(located(message, location).with_code(super::codes::NOT_CHECKED));
                } else {
                    self.error(message, location);
                }
                return Vec::new();
            }
        };
        let canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        if let Some(pos) = self.stack.iter().position(|(p, _)| *p == canonical) {
            let mut chain: Vec<String> = self.stack[pos..].iter().map(|(_, r)| r.clone()).collect();
            chain.push(call.uses.clone());
            self.error(
                format!(
                    "{in_file}sequence call `{label}` is a cycle: {}",
                    chain.join(" -> ")
                ),
                location,
            );
            return Vec::new();
        }

        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: cannot read {}: {e}",
                        self.shown(&file)
                    ),
                    location,
                );
                return Vec::new();
            }
        };

        let (seq, written_entries, inputs) =
            match self.parse_sequence_file(&text, &file, call, &label, &location) {
                Some(parsed) => parsed,
                None => return Vec::new(),
            };

        let instance_key = call.instance_key(&seq.name);
        if !crate::python::is_valid_python_identifier(&instance_key) {
            self.error(
                format!(
                    "{in_file}sequence call `{label}`: key `{instance_key}` is not a Python identifier (letters, digits and _, not starting with a digit)"
                ),
                location.clone(),
            );
        }
        let name = call.name.clone().unwrap_or_else(|| seq.name.clone());

        // The bound phases and plugs have. The server refuses a longer name
        // at upload, and the CLI would park the whole run as unretryable.
        for (what, value) in [("name", name.as_str()), ("key", instance_key.as_str())] {
            let len = value.chars().count();
            if !(1..=100).contains(&len) {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: {what} must be 1 to 100 characters, got {len}"
                    ),
                    location.clone(),
                );
            }
        }

        // The call's `then:` shares the phases' type, which also knows
        // `pass:` and `retry`. Neither has a meaning on a call (a sequence
        // is not re-run, and a passing call never stops anything), so they
        // are refused here rather than silently read as continue.
        if let Some(then) = &call.then {
            let retries = [&then.fail, &then.error, &then.timeout]
                .iter()
                .any(|a| **a == Some(crate::procedure::schema::PhaseNextAction::Retry));
            if then.pass.is_some() || retries {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: `then:` on a sequence call takes fail, error or timeout, each continue or stop"
                    ),
                    location.clone(),
                );
            }
        }

        let plug_bindings = self.bind_plugs(&seq, call, frame, &label, &in_file, &location);

        let mut path = frame.path.clone();
        path.push(instance_key.clone());
        let child_scope = if frame.top_level {
            call.scope
        } else {
            frame.scope
        };
        let base_dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.procedure_dir.to_path_buf());
        // One stage per enclosing SEQUENCE: the procedure's own list is
        // not one (the phase's `StageScope` already says it), so a call made
        // in the procedure starts the chain empty.
        let child_stages: Vec<PhaseStage> = if frame.top_level {
            Vec::new()
        } else {
            frame.stages.iter().copied().chain([stage]).collect()
        };
        let child = Frame {
            path: path.clone(),
            stages: child_stages,
            rel_dir: relative_dir(self.python_root, &base_dir),
            base_dir,
            file: Some(file.clone()),
            inputs: inputs.clone(),
            plug_aliases: plug_bindings
                .iter()
                .map(|(req, bound)| {
                    // Through nested calls the binding chains down to the
                    // procedure's plug: the parent's alias of the bound name,
                    // or the bound name itself at the top.
                    let target = frame
                        .plug_aliases
                        .get(bound)
                        .cloned()
                        .unwrap_or_else(|| bound.clone());
                    (req.clone(), target)
                })
                .collect(),
            available_plugs: seq.plugs.iter().map(|p| p.key()).collect(),
            scope: child_scope,
            top_level: false,
            location: location.clone(),
            value_scope: Some(super::values::ValueScope {
                config: seq.config.keys().cloned().collect(),
                secrets: seq.secrets.keys().cloned().collect(),
            }),
        };

        let instance_index = self.instances.len();
        self.entry_instances
            .insert((frame.path.clone(), stage, index), instance_index);
        self.instances.push(SequenceInstance {
            path: path.clone(),
            name,
            source: call.uses.clone(),
            file: file.display().to_string(),
            description: as_written.description.clone(),
            file_description: (!seq.description.is_empty()).then(|| seq.description.clone()),
            stage,
            entry_index: index,
            scope: child_scope.filter(|_| frame.top_level),
            then: call.then.clone(),
            enabled: call.enabled,
            depends_on: Vec::new(),
            inputs,
            config: seq.config.clone(),
            secrets: seq.secrets.clone(),
            plugs: plug_bindings,
            with: as_written.with.clone(),
            plug_bindings: as_written.plugs.clone(),
            depends_on_written: as_written.depends_on.clone(),
            name_written: as_written.name.clone(),
            written_form: frame.top_level || written.is_some(),
            key_stated: call.key.is_some(),
            setup_keys: Vec::new(),
            main_keys: Vec::new(),
            teardown_keys: Vec::new(),
        });

        self.stack.push((canonical, call.uses.clone()));
        let mut setup_phases = self.expand_list(
            &seq.setup,
            &child,
            PhaseStage::Setup,
            Some(&written_entries.setup),
        );
        let mut main_phases = self.expand_list(
            &seq.main,
            &child,
            PhaseStage::Main,
            Some(&written_entries.main),
        );
        let mut teardown_phases = self.expand_list(
            &seq.teardown,
            &child,
            PhaseStage::Teardown,
            Some(&written_entries.teardown),
        );
        self.stack.pop();

        self.check_level_keys(
            &[&setup_phases, &main_phases, &teardown_phases],
            &child,
            &[
                (&seq.setup, PhaseStage::Setup),
                (&seq.main, PhaseStage::Main),
                (&seq.teardown, PhaseStage::Teardown),
            ],
        );
        self.resolve_call_dependencies(&mut setup_phases, &child);
        self.resolve_call_dependencies(&mut main_phases, &child);
        self.resolve_call_dependencies(&mut teardown_phases, &child);

        if setup_phases.is_empty() && main_phases.is_empty() && teardown_phases.is_empty() {
            self.error(
                format!(
                    "{in_file}sequence call `{label}`: {} has no phases",
                    self.shown(&file)
                ),
                location.clone(),
            );
        }

        // The sequence's own barriers, as explicit dependencies: its main
        // waits for all of its setup, its teardown for its setup and main.
        // Nested calls' phases are in these lists already, so a nested
        // sequence sits inside the barriers of the one that calls it.
        let setup_keys: Vec<String> = setup_phases.iter().map(|p| p.key.clone()).collect();
        let main_keys: Vec<String> = main_phases.iter().map(|p| p.key.clone()).collect();
        let teardown_keys: Vec<String> = teardown_phases.iter().map(|p| p.key.clone()).collect();
        for phase in &mut main_phases {
            phase.depends_on.extend(setup_keys.iter().cloned());
        }
        for phase in &mut teardown_phases {
            phase
                .depends_on
                .extend(setup_keys.iter().chain(main_keys.iter()).cloned());
        }

        // The call's own `depends_on` applies to every phase it brings in,
        // spelled in the parent's frame (prefixed, and possibly naming a
        // sibling call, which the parent's level resolves).
        let parent_prefix = frame.path.join("/");
        let call_deps: Vec<String> = call
            .depends_on
            .iter()
            .map(|d| {
                if parent_prefix.is_empty() {
                    d.clone()
                } else {
                    format!("{parent_prefix}/{d}")
                }
            })
            .collect();

        let mut phases: Vec<PhaseDefinition> = setup_phases
            .into_iter()
            .chain(main_phases)
            .chain(teardown_phases)
            .collect();
        for phase in &mut phases {
            phase.depends_on.extend(call_deps.iter().cloned());
            if !call.enabled {
                phase.enabled = false;
            }
            dedup_in_place(&mut phase.depends_on);
        }

        let instance = &mut self.instances[instance_index];
        instance.depends_on = call_deps;
        instance.setup_keys = setup_keys;
        instance.main_keys = main_keys;
        instance.teardown_keys = teardown_keys;

        phases
    }

    /// The file as a `SequenceYaml`, read by the procedure's own reader with
    /// the `{inputs.x}` markers substituted in the text first (so every
    /// other scalar keeps its spelling, `name: 3.10` included), plus the
    /// file's entries as written, for the editors of the file (Studio), see
    /// `written_lists`. `None` after reporting what is wrong, with the
    /// sequence file's position in the message: Studio applies an error's
    /// own line to the procedure file, where the call's entry (`location`)
    /// is the place to point at.
    fn parse_sequence_file(
        &mut self,
        text: &str,
        file: &Path,
        call: &SequenceCallYaml,
        label: &str,
        location: &Option<RefLocation>,
    ) -> Option<(
        SequenceYaml,
        WrittenLists,
        BTreeMap<String, serde_json::Value>,
    )> {
        let shown = self.shown(file);

        // Inputs first, from the untouched text: they are what the markers
        // in the rest of it refer to. This read also reports a text that
        // does not parse at all, or is not a mapping (an empty file, a list).
        // An empty file parses as nothing and a list as a list: neither is
        // a sequence file, said as such rather than as a type error. A text
        // that does not parse at all is left to the typed read below.
        let is_mapping = !text.trim().is_empty()
            && super::yaml::from_str::<serde_json::Value>(text)
                .map(|doc| doc.is_object())
                .unwrap_or(true);
        if !is_mapping {
            self.error(
                format!("{shown}: a sequence file is a YAML mapping with name and phases (call `{label}`)"),
                location.clone(),
            );
            return None;
        }
        // What the procedure file still loads with a deprecation warning
        // (`.inf`, `.nan`, the repaired layouts) is refused in a sequence
        // file: the format is new and nobody relies on the old spellings.
        // Checked on the text as written, before anything else reads it: an
        // input default of `.inf` would otherwise fail its type first, and
        // the raw parser the rewriter runs refuses the repaired layouts. The
        // refusal must name the spelling and its line.
        // A run-start marker outside `with:` and `env:`, or passed to a
        // nested call: refused at its line, like a deprecation. The old
        // slot spellings are deprecations, refused with the parser's.
        let text_pass =
            super::value_checks::text_diagnostics(text, super::value_checks::FileKind::Sequence);
        let mut deprecations = super::yaml::parser_warnings(text);
        deprecations.extend(text_pass.warnings.iter().cloned());
        if self.refuse_deprecations(&deprecations, &shown, label, location) {
            return None;
        }
        for error in &text_pass.errors {
            self.error_in_file(
                format!(
                    "{shown}: line {}, column {}: {}: {} (call `{label}`)",
                    error.line, error.column, error.path, error.message
                ),
                location.clone(),
            );
        }
        if !text_pass.errors.is_empty() {
            return None;
        }
        let declared = match super::sequence::read_declared_inputs(text) {
            Ok(declared) => declared,
            Err(e) => {
                self.error(format!("{shown}: {e} (call `{label}`)"), location.clone());
                return None;
            }
        };
        let inputs = self.resolve_inputs(&declared, call, label, &shown, location)?;

        // The markers, substituted in the text: a whole marker becomes the
        // input's JSON value (valid YAML flow, so a number stays a number),
        // an embedded one interpolates into a quoted string. The `inputs:`
        // declarations are not substituted.
        let mut unknown = Vec::new();
        let substituted = match super::yaml::rewrite_value_scalars(text, |scalar| {
            marker_rewrite(scalar, &inputs, &mut unknown)
        }) {
            Ok(text) => text,
            // The declarations read and the deprecations are refused above,
            // so the raw text parses: what is left is a scalar the rewriter
            // cannot stand in for. Defensive: every scalar form the parser
            // emits is covered, and no file has reached this yet. The file
            // is never read with its markers left as written.
            Err(super::yaml::RewriteError::Scalar { line }) if text.contains("{inputs.") => {
                self.error(
                    format!(
                        "{shown}: line {line}: a `{{inputs.x}}` marker sits where it cannot be substituted; write the value as a plain, quoted or block scalar (call `{label}`)"
                    ),
                    location.clone(),
                );
                return None;
            }
            Err(_) => text.to_string(),
        };
        unknown.sort();
        unknown.dedup();
        for name in &unknown {
            let near = super::pysource::closest(name, declared.keys().map(String::as_str))
                .map(|n| format!("; did you mean `{{inputs.{n}}}`?"))
                .unwrap_or_default();
            self.error(
                format!(
                    "{shown}: `{{inputs.{name}}}` names an input the file does not declare (declared: {}){near}",
                    if declared.is_empty() {
                        "none".to_string()
                    } else {
                        declared.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                ),
                location.clone(),
            );
        }
        // A marker left as written then fails the typed read of its field
        // (`>=` needs a number): a side effect of the error above.
        if !unknown.is_empty() {
            return None;
        }

        let (seq, warnings) =
            match super::yaml::from_str_with_warnings::<SequenceYaml>(&substituted) {
                Ok(parsed) => parsed,
                Err(e) => {
                    self.error_in_file(
                        format!("{shown}: {} (call `{label}`)", friendly_sequence_error(&e)),
                        location.clone(),
                    );
                    return None;
                }
            };
        // A substituted value cannot bring a deprecation in (a text input is
        // written quoted), so this is the check above on the text as read,
        // kept as a belt. A replacement never holds a line break, so the
        // lines are the file's own.
        if self.refuse_deprecations(&warnings, &shown, label, location) {
            return None;
        }
        let declaration_problems =
            super::value_checks::check_declarations(&seq.config, &seq.secrets);
        for problem in &declaration_problems {
            self.error_in_file(
                format!("{shown}: {problem} (call `{label}`)"),
                location.clone(),
            );
        }
        if !declaration_problems.is_empty() {
            return None;
        }
        Some((seq, written_lists(text, &inputs), inputs))
    }

    /// A sequence file as the procedure's author knows it: relative to
    /// the procedure's folder.
    fn shown(&self, file: &Path) -> String {
        file.strip_prefix(self.procedure_dir)
            .unwrap_or(file)
            .display()
            .to_string()
    }

    /// The deprecation refusal of a sequence file, one error per warning
    /// with the file, line and column. True when there was something to
    /// refuse.
    fn refuse_deprecations(
        &mut self,
        warnings: &[super::yaml::Warning],
        shown: &str,
        label: &str,
        location: &Option<RefLocation>,
    ) -> bool {
        // Deprecations only: a notice (a secret in `env:`) is no old
        // spelling to refuse.
        let warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.message.contains(super::yaml::DEPRECATION_NOTICE))
            .collect();
        for warning in &warnings {
            self.error_in_file(
                format!(
                    "{shown}: line {}, column {}: {} (call `{label}`)",
                    warning.line,
                    warning.column,
                    warning.message.replace(
                        super::yaml::DEPRECATION_NOTICE,
                        "A sequence file does not accept it."
                    )
                ),
                location.clone(),
            );
        }
        !warnings.is_empty()
    }

    fn resolve_inputs(
        &mut self,
        declared: &BTreeMap<String, InputSpecYaml>,
        call: &SequenceCallYaml,
        label: &str,
        shown: &str,
        location: &Option<RefLocation>,
    ) -> Option<BTreeMap<String, serde_json::Value>> {
        let mut ok = true;
        let mut values = BTreeMap::new();
        for (name, spec) in declared {
            if !crate::python::is_valid_python_identifier(name) {
                self.error(
                    format!("{shown}: input `{name}` is not a Python identifier; phases read inputs as `inputs.<name>`"),
                    location.clone(),
                );
                ok = false;
            }
            let value = match call.with.get(name) {
                Some(v) => v.clone(),
                None => match &spec.default {
                    Some(d) => d.clone(),
                    None => {
                        self.error(
                            format!(
                                "sequence call `{label}`: input `{name}` of {shown} is required, set it in `with:`"
                            ),
                            location.clone(),
                        );
                        ok = false;
                        continue;
                    }
                },
            };
            if !spec.input_type.accepts(&value) {
                let from = if call.with.contains_key(name) {
                    "with:"
                } else {
                    "its default"
                };
                self.error(
                    format!(
                        "sequence call `{label}`: input `{name}` of {shown} is a {} but {from} gives {}",
                        spec.input_type.as_str(),
                        describe_json(&value)
                    ),
                    if call.with.contains_key(name) {
                        at_with(location, name)
                    } else {
                        location.clone()
                    },
                );
                ok = false;
                continue;
            }
            // `accepts` lets a whole float (`10.0`, `1e3`) through for an
            // integer input; store the integer, or the phase would receive
            // a float and a `{inputs.n}` marker in an integer field would
            // fail to deserialize right after the check accepted it.
            let value = match (&spec.input_type, value.as_f64()) {
                (crate::procedure::sequence::InputType::Integer, Some(f))
                    if !value.is_i64() && !value.is_u64() =>
                {
                    serde_json::Value::from(f as i64)
                }
                _ => value,
            };
            values.insert(name.clone(), value);
        }
        for name in call.with.keys() {
            if !declared.contains_key(name) {
                let hint = super::pysource::closest(
                    name,
                    declared
                        .keys()
                        .filter(|k| !call.with.contains_key(*k))
                        .map(String::as_str),
                )
                .map(|alt| format!("; did you mean `{alt}`?"))
                .unwrap_or_default();
                self.error(
                    format!(
                        "sequence call `{label}`: `with.{name}` is not an input of {shown} (declared: {}){hint}",
                        if declared.is_empty() {
                            "none".to_string()
                        } else {
                            declared.keys().cloned().collect::<Vec<_>>().join(", ")
                        }
                    ),
                    at_with(location, name),
                );
                ok = false;
            }
        }
        ok.then_some(values)
    }

    /// Requirement key to the key it is bound to in the calling frame.
    fn bind_plugs(
        &mut self,
        seq: &SequenceYaml,
        call: &SequenceCallYaml,
        frame: &Frame,
        label: &str,
        in_file: &str,
        location: &Option<RefLocation>,
    ) -> BTreeMap<String, String> {
        let mut bindings = BTreeMap::new();
        // Same reserved name as for a phase: a requirement keyed `dashboard`
        // is reached through the plug aliases, after the worker has filled
        // the parameter with the API client.
        for requirement in &seq.plugs {
            if requirement.key().eq_ignore_ascii_case("dashboard") {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: plug requirement `{}` has key `dashboard`, which is the parameter a phase receives the TofuPilot API client through: a phase naming it would get the client, never the plug. Set another `key:`",
                        requirement.name
                    ),
                    location.clone(),
                );
            }
        }
        let required: Vec<String> = seq.plugs.iter().map(|p| p.key()).collect();
        let available = |key: &str| frame.available_plugs.iter().any(|k| k == key);
        for key in &required {
            let bound = match call.plugs.get(key) {
                Some(target) => target.clone(),
                None if available(key) => key.clone(),
                None => {
                    self.error(
                        format!(
                            "{in_file}sequence call `{label}`: plug `{key}` required by the sequence is not bound; add `plugs: {{{key}: <plug key>}}` on the call, or declare a plug with key `{key}`{}",
                            if frame.available_plugs.is_empty() {
                                String::new()
                            } else {
                                format!(" (available: {})", frame.available_plugs.join(", "))
                            }
                        ),
                        location.clone(),
                    );
                    continue;
                }
            };
            if !available(&bound) {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: `plugs.{key}` binds to `{bound}`, which is not a plug of {} (available: {})",
                        if frame.top_level { "the procedure" } else { "the enclosing sequence" },
                        if frame.available_plugs.is_empty() {
                            "none".to_string()
                        } else {
                            frame.available_plugs.join(", ")
                        }
                    ),
                    location.clone(),
                );
                continue;
            }
            bindings.insert(key.clone(), bound);
        }
        for key in call.plugs.keys() {
            if !required.contains(key) {
                self.error(
                    format!(
                        "{in_file}sequence call `{label}`: `plugs.{key}` names no plug requirement of the sequence (required: {})",
                        if required.is_empty() {
                            "none".to_string()
                        } else {
                            required.join(", ")
                        }
                    ),
                    location.clone(),
                );
            }
        }
        bindings
    }

    /// Keys at one level share a namespace: the local keys of the phases
    /// written in the file and the instance keys of its calls. Two entries
    /// with one key would make `depends_on` ambiguous and, for phases,
    /// collide in the job graph.
    fn check_level_keys(
        &mut self,
        lists: &[&Vec<PhaseDefinition>],
        frame: &Frame,
        entries: &[(&Vec<PhaseEntryYaml>, PhaseStage)],
    ) {
        let depth = frame.path.len();
        let in_file = frame.in_file();
        let mut seen: HashSet<String> = HashSet::new();
        let mut reported: HashSet<String> = HashSet::new();
        // Phases written at this level (not brought in by a call). At the
        // procedure level a phase repeating a phase's key is the loader's
        // to report, on `main` only as it always was: adding a call must
        // not refuse a procedure that loaded without one. A call's key
        // clashing with anything is new, and reported below.
        for phases in lists {
            for phase in phases.iter().filter(|p| p.sequence_path.len() == depth) {
                let key = phase.local_key().to_string();
                let fresh = seen.insert(key.clone());
                if !fresh && !frame.top_level && reported.insert(key.clone()) {
                    self.error(
                        format!("{in_file}duplicate key `{key}`: every phase and sequence call of a file needs its own key"),
                        frame.location.clone(),
                    );
                }
            }
        }
        for (list, stage) in entries {
            for (index, entry) in list.iter().enumerate() {
                if !matches!(entry, PhaseEntryYaml::Uses(_)) {
                    continue;
                }
                // The instance key is on the instance once expanded.
                let key = self
                    .entry_instances
                    .get(&(frame.path.clone(), *stage, index))
                    .and_then(|i| self.instances.get(*i))
                    .map(|i| i.key().to_string());
                let Some(key) = key else { continue };
                if !seen.insert(key.clone()) && reported.insert(key.clone()) {
                    let location = if frame.top_level {
                        Some(RefLocation::key(ref_list(*stage), index, "key"))
                    } else {
                        frame.location.clone()
                    };
                    self.error(
                        format!("{in_file}duplicate key `{key}`: every phase and sequence call of a file needs its own key (set `key:` on one of them)"),
                        location,
                    );
                }
            }
        }
    }

    /// A `depends_on` naming a sequence call at the same level waits for
    /// that call's terminal phases, which through the call's own barriers
    /// means for the whole call.
    fn resolve_call_dependencies(&mut self, phases: &mut [PhaseDefinition], frame: &Frame) {
        let depth = frame.path.len();
        let terminals: HashMap<String, Vec<String>> = self
            .instances
            .iter()
            .filter(|i| i.path.len() == depth + 1 && i.path[..depth] == frame.path[..])
            .map(|i| (i.path_str(), i.terminal_keys().to_vec()))
            .collect();
        if terminals.is_empty() {
            return;
        }
        for phase in phases.iter_mut() {
            let mut resolved = Vec::new();
            for dep in &phase.depends_on {
                match terminals.get(dep) {
                    Some(keys) => resolved.extend(keys.iter().cloned()),
                    None => resolved.push(dep.clone()),
                }
            }
            dedup_in_place(&mut resolved);
            phase.depends_on = resolved;
        }
        for instance in self
            .instances
            .iter_mut()
            .filter(|i| i.path.len() == depth + 1 && i.path[..depth] == frame.path[..])
        {
            let mut resolved = Vec::new();
            for dep in &instance.depends_on {
                match terminals.get(dep) {
                    Some(keys) => resolved.extend(keys.iter().cloned()),
                    None => resolved.push(dep.clone()),
                }
            }
            dedup_in_place(&mut resolved);
            instance.depends_on = resolved;
        }
    }
}

fn dedup_in_place(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|k| seen.insert(k.clone()));
}

/// A `python:` reference written in a sequence file, moved into the
/// procedure directory's frame of reference: the worker joins module
/// paths onto the procedure directory, so `phases.zero` in
/// `sequences/calib/x.yaml` becomes `sequences/calib/phases/zero.py:zero`.
/// Absolute and `~` specs are left alone (the lint refuses them).
pub fn rebase_python_spec(spec: &PythonSpec, rel_dir: &str) -> PythonSpec {
    let raw = spec.as_str().trim();
    if rel_dir.is_empty() || raw.is_empty() {
        return spec.clone();
    }
    // Keep a Windows drive letter with its path.
    let (path_part, callable) = match raw.split_once(':') {
        Some((p, c)) if p.len() == 1 && p.chars().all(|ch| ch.is_ascii_alphabetic()) => {
            match c.split_once(':') {
                Some((rest, callable)) => {
                    (format!("{p}:{rest}"), Some(callable.trim().to_string()))
                }
                None => (format!("{p}:{c}"), None),
            }
        }
        Some((p, c)) => (p.trim().to_string(), Some(c.trim().to_string())),
        None => (raw.to_string(), None),
    };
    if Path::new(&path_part).is_absolute()
        || path_part.starts_with('~')
        || path_part.starts_with('/')
    {
        return spec.clone();
    }
    let is_file_path = path_part.contains('/') || path_part.contains('\\');
    let file_path = if is_file_path {
        let trimmed = path_part.trim_start_matches("./");
        let with_ext = if trimmed.ends_with(".py") {
            trimmed.to_string()
        } else {
            format!("{trimmed}.py")
        };
        format!("{rel_dir}/{with_ext}")
    } else {
        let leading_dots = path_part.len() - path_part.trim_start_matches('.').len();
        let module = if leading_dots > 0 {
            // The worker's spelling of a dot-relative module: under
            // `phases/`, one `..` per extra dot.
            format!(
                "phases/{}{}",
                "../".repeat(leading_dots - 1),
                path_part[leading_dots..].replace('.', "/")
            )
        } else {
            path_part.replace('.', "/")
        };
        format!("{rel_dir}/{module}.py")
    };
    let callable = callable.unwrap_or_else(|| {
        path_part
            .rsplit(['.', '/', '\\'])
            .next()
            .unwrap_or(&path_part)
            .trim_end_matches(".py")
            .to_string()
    });
    PythonSpec::new(format!("{file_path}:{callable}"))
}

/// Whether a sequence phase's `python:` names a module installed in the
/// environment rather than a file of the sequence's tree: dotted, with no
/// file where the rebase points, nothing in the sequence file's directory
/// for its first segment (`shared_lib` in `shared_lib.calibration:zero`),
/// and no file or package for it under the Python root either. The worker
/// resolves a kept spec as a file under that root before importing it, so
/// a module the calling procedure happens to have would run in the
/// sequence's place; such a spec is rebased and the reference check
/// reports the sequence's own missing file.
fn names_installed_module(
    spec: &PythonSpec,
    rebased: &PythonSpec,
    sequence_dir: &Path,
    python_root: &Path,
) -> bool {
    let raw = spec.as_str();
    if raw.contains(['/', '\\']) {
        return false;
    }
    let module = raw.split_once(':').map_or(raw, |(m, _)| m);
    let first = module
        .trim_start_matches('.')
        .split('.')
        .next()
        .unwrap_or("");
    if first.is_empty() || module.starts_with('.') {
        return false;
    }
    let file_part = rebased
        .as_str()
        .rsplit_once(':')
        .map_or(rebased.as_str(), |(f, _)| f);
    let as_path = module.replace('.', "/");
    !python_root.join(file_part).exists()
        && !sequence_dir.join(first).exists()
        && !sequence_dir.join(format!("{first}.py")).exists()
        && !python_root.join(format!("{as_path}.py")).exists()
        && !python_root.join(&as_path).exists()
}

/// The file's entries as written, list by list, for the editors of the
/// file: the file's own text is typed, so every spelling is the file's. An
/// entry with a marker in a typed field (`limit: "{inputs.n}"`) only types
/// once that marker is substituted: the typed read fails at the marker, the
/// marker alone is substituted and the read retried, and that entry gets
/// `None`. Everything `None` when the text fails for another reason, which
/// the typed read of the substituted text reports.
#[derive(Default)]
struct WrittenLists {
    setup: Vec<Option<PhaseEntryYaml>>,
    main: Vec<Option<PhaseEntryYaml>>,
    teardown: Vec<Option<PhaseEntryYaml>>,
}

fn written_lists(text: &str, inputs: &BTreeMap<String, serde_json::Value>) -> WrittenLists {
    let mut current = text.to_string();
    let mut touched: HashSet<(String, usize)> = HashSet::new();
    let mut last: Option<(u32, u32)> = None;
    // One retry per marker in a typed field, a few at most.
    for _ in 0..64 {
        let err = match super::yaml::from_str::<SequenceYaml>(&current) {
            Ok(seq) => {
                let take = |list: &str, entries: Vec<PhaseEntryYaml>| {
                    entries
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            (!touched.contains(&(list.to_string(), index))).then_some(entry)
                        })
                        .collect()
                };
                return WrittenLists {
                    setup: take("setup", seq.setup),
                    main: take("main", seq.main),
                    teardown: take("teardown", seq.teardown),
                };
            }
            Err(err) => err,
        };
        let Some(at) = err.line().zip(err.column()) else {
            break;
        };
        if last == Some(at) {
            break;
        }
        last = Some(at);
        // The marker scalar the read stopped on: by line and column, else
        // by line when it is the only marker there (the column of a type
        // error may sit a character off the scalar's start).
        let mut hit = None;
        let mut on_line = 0;
        let exact = super::yaml::rewrite_value_scalars(&current, |scalar| {
            if scalar.line as u32 != at.0 {
                return None;
            }
            if scalar.value.contains("{inputs.") {
                on_line += 1;
            }
            if scalar.column as u32 != at.1 {
                return None;
            }
            let replacement = marker_rewrite(scalar, inputs, &mut Vec::new())?;
            hit = entry_of(&scalar.path);
            Some(replacement)
        })
        .ok();
        let rewritten = match (exact, hit) {
            (Some(next), Some(entry)) => Some((next, entry)),
            _ if on_line == 1 => {
                let mut hit = None;
                super::yaml::rewrite_value_scalars(&current, |scalar| {
                    if scalar.line as u32 != at.0 {
                        return None;
                    }
                    let replacement = marker_rewrite(scalar, inputs, &mut Vec::new())?;
                    hit = entry_of(&scalar.path);
                    Some(replacement)
                })
                .ok()
                .zip(hit)
            }
            _ => None,
        };
        match rewritten {
            Some((next, entry)) => {
                current = next;
                touched.insert(entry);
            }
            None => break,
        }
    }
    WrittenLists::default()
}

/// The list and index of a path's entry: `main[1].retry.limit` is entry 1
/// of `main`.
fn entry_of(path: &str) -> Option<(String, usize)> {
    let (list, rest) = path.split_once('[')?;
    let (index, _) = rest.split_once(']')?;
    Some((list.to_string(), index.parse().ok()?))
}

/// The replacement of a value scalar holding `{inputs.x}` markers: a whole
/// marker becomes the input's typed value (as JSON, which is valid YAML
/// flow); a marker inside a string is interpolated as text, quoted. Nothing
/// else in a string is touched: `{slot}`, `{slot_name}` and regex
/// quantifiers like `{3}` survive. The `inputs:` declarations are never
/// substituted. Unknown names are collected in `unknown`; the scalar stays.
fn marker_rewrite(
    scalar: &super::yaml::ValueScalar<'_>,
    inputs: &BTreeMap<String, serde_json::Value>,
    unknown: &mut Vec<String>,
) -> Option<String> {
    if scalar.path == "inputs" || scalar.path.starts_with("inputs.") {
        return None;
    }
    if let Some(name) = whole_marker(scalar.value) {
        return match inputs.get(name) {
            Some(value) => Some(serde_json::to_string(value).unwrap_or_else(|_| "null".into())),
            None => {
                unknown.push(name.to_string());
                None
            }
        };
    }
    if !scalar.value.contains("{inputs.") {
        return None;
    }
    let interpolated = interpolate_markers(scalar.value, inputs, unknown);
    (interpolated != scalar.value)
        .then(|| serde_json::to_string(&interpolated).unwrap_or_else(|_| "\"\"".into()))
}

/// `Some(name)` when `s` is exactly `{inputs.<identifier>}`.
fn whole_marker(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{inputs.")?.strip_suffix('}')?;
    crate::python::is_valid_python_identifier(inner).then_some(inner)
}

fn interpolate_markers(
    s: &str,
    inputs: &BTreeMap<String, serde_json::Value>,
    unknown: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{inputs.") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{inputs.".len()..];
        match after.find('}') {
            Some(end) if crate::python::is_valid_python_identifier(&after[..end]) => {
                let name = &after[..end];
                match inputs.get(name) {
                    Some(v) => out.push_str(&display_json(v)),
                    None => {
                        unknown.push(name.to_string());
                        out.push_str(&rest[start..start + "{inputs.".len() + end + 1]);
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                // Not a marker: keep the text and move past the brace.
                out.push_str("{inputs.");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// An input as text inside a string: strings raw, the rest as JSON.
fn display_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn describe_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => format!("the boolean {b}"),
        serde_json::Value::Number(n) => format!("the number {n}"),
        serde_json::Value::String(s) => format!("the string {s:?}"),
        serde_json::Value::Array(_) => "a list".to_string(),
        serde_json::Value::Object(_) => "a mapping".to_string(),
    }
}

/// A sequence file's parse failure, positioned in that file: the reader
/// keeps the position out of its message, and the error is reported at the
/// call, so the message is the only place it can go. A procedure-only key
/// is reworded for the author of a sequence file.
fn friendly_sequence_error(e: &super::yaml::Error) -> String {
    let mut message = e.to_string();
    if let Some(renamed) = super::values::renamed_top_level_block(e) {
        message = renamed.to_string();
    }
    for (key, why) in [
        (
            "version",
            "a sequence has no version of its own, the procedure's applies",
        ),
        (
            "execution",
            "slots, workers and on_first_failure are the procedure's; a sequence inherits them",
        ),
        (
            "unit",
            "the unit is identified by the procedure, once, before any phase",
        ),
        (
            "operated_by",
            "the operator is a run property, declared by the procedure",
        ),
        (
            "dashboard",
            "every phase of a run that uploads receives the `dashboard` client, nothing to declare",
        ),
        (
            "env",
            "the procedure's env: applies to every phase; set env: on a phase of this file",
        ),
    ] {
        if message.contains(&format!("unknown field `{key}`")) {
            message = format!("`{key}:` is not allowed in a sequence file: {why}");
            break;
        }
    }
    // In front, like the deprecation refusals: `s.yaml: line 5, column 3: ...`.
    match (e.line(), e.column()) {
        (Some(line), Some(column)) => format!("line {line}, column {column}: {message}"),
        (Some(line), None) => format!("line {line}: {message}"),
        _ => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_reference_shape() {
        assert_eq!(
            split_package_reference("acme_testlib:sequences/calib.yaml"),
            Some(("acme_testlib", "sequences/calib.yaml"))
        );
        assert_eq!(
            split_package_reference("acme.testlib:x.yaml"),
            Some(("acme.testlib", "x.yaml"))
        );
        // A Windows drive is a path, not a package.
        assert_eq!(split_package_reference("C:/seq/x.yaml"), None);
        // A relative path is not a package reference.
        assert_eq!(split_package_reference("./sequences/x.yaml"), None);
        assert_eq!(split_package_reference("no-dash:x.yaml"), None);
    }

    #[test]
    fn whole_and_embedded_markers() {
        let mut inputs = BTreeMap::new();
        inputs.insert("target_voltage".to_string(), serde_json::json!(12.5));
        inputs.insert("label".to_string(), serde_json::json!("DMM"));
        let mut unknown = Vec::new();
        let scalar = |value: &'static str| super::super::yaml::ValueScalar {
            value,
            path: "main[0].x".to_string(),
            line: 1,
            column: 1,
        };

        // A whole marker keeps the input's type.
        assert_eq!(
            marker_rewrite(&scalar("{inputs.target_voltage}"), &inputs, &mut unknown),
            Some("12.5".to_string())
        );
        assert_eq!(
            marker_rewrite(&scalar("{inputs.label}"), &inputs, &mut unknown),
            Some("\"DMM\"".to_string())
        );
        // An embedded one interpolates into a quoted string.
        assert_eq!(
            marker_rewrite(
                &scalar("Check {inputs.target_voltage} V on {inputs.label}"),
                &inputs,
                &mut unknown
            ),
            Some("\"Check 12.5 V on DMM\"".to_string())
        );
        // Other braces are not markers.
        assert_eq!(
            marker_rewrite(
                &scalar("[0-9]{3} {slot} {slot_name} {inputs}"),
                &inputs,
                &mut unknown
            ),
            None
        );
        assert!(unknown.is_empty());
        // The declarations are never substituted.
        let declared = super::super::yaml::ValueScalar {
            value: "{inputs.label}",
            path: "inputs.label.default".to_string(),
            line: 1,
            column: 1,
        };
        assert_eq!(marker_rewrite(&declared, &inputs, &mut unknown), None);

        assert_eq!(
            marker_rewrite(&scalar("{inputs.nope}"), &inputs, &mut unknown),
            None
        );
        assert_eq!(unknown, vec!["nope".to_string()]);
    }

    #[test]
    fn python_spec_rebased_into_the_procedure_frame() {
        let r = |s: &str| {
            rebase_python_spec(&PythonSpec::new(s), "sequences/calib")
                .as_str()
                .to_string()
        };
        assert_eq!(r("phases.zero"), "sequences/calib/phases/zero.py:zero");
        assert_eq!(
            r("phases.zero:check"),
            "sequences/calib/phases/zero.py:check"
        );
        assert_eq!(
            r("./phases/zero.py:check"),
            "sequences/calib/phases/zero.py:check"
        );
        assert_eq!(r("phases/zero"), "sequences/calib/phases/zero.py:zero");
        assert_eq!(r(".zero"), "sequences/calib/phases/zero.py:zero");
        // Untouched at the top level.
        assert_eq!(
            rebase_python_spec(&PythonSpec::new("phases.zero"), "").as_str(),
            "phases.zero"
        );
        // A parent directory in the frame stays a parent directory.
        assert_eq!(
            rebase_python_spec(&PythonSpec::new("phases.zero"), "../shared/seq").as_str(),
            "../shared/seq/phases/zero.py:zero"
        );
    }

    #[test]
    fn relative_dir_walks_up_and_down() {
        let root = std::env::temp_dir().join(format!("tp-reldir-{}", uuid::Uuid::new_v4()));
        let proc_dir = root.join("procedures/a");
        let shared = root.join("shared/seq");
        std::fs::create_dir_all(&proc_dir).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        assert_eq!(relative_dir(&proc_dir, &shared), "../../shared/seq");
        assert_eq!(
            relative_dir(&proc_dir, &proc_dir.join("sequences")),
            "sequences"
        );
        assert_eq!(relative_dir(&proc_dir, &proc_dir), "");
        std::fs::remove_dir_all(&root).ok();
    }
}
