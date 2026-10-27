use base64::Engine;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::process::ChildStderr;
use tokio::sync::RwLock;

use crate::event_sink::{EventSink, ExecutionEvent};
use crate::job::{Job, JobResult};
use crate::plugs::process::ChildProcess;
use crate::protocol;
use crate::transport;

/// Best-effort mimetype for an attachment from its filename. The
/// operator-UI gates image rendering on an `image/*` mimetype, so an
/// attachment with no mimetype never renders even when its bytes are an
/// image. `attach.data`/`attach_file` don't carry a content type, so we
/// derive one from the extension. Falls back to `application/octet-stream`
/// for unknown extensions so the upload PUT always carries a real
/// Content-Type (the server reads it back via HeadObject on finalize).
fn guess_attachment_mimetype(name: &str) -> Option<String> {
    Some(
        mime_guess::from_path(name)
            .first()
            .map(|m| m.essence_str().to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string()),
    )
}

/// Write attachment bytes into the caller-supplied dir with a collision-safe
/// `<uuid8>_<name>` filename and return the stored path. Mirrors the naming
/// the removed ReportManager used so the kiosk `/attachments/*` route and the
/// CLI upload queue resolve the same basename.
fn store_attachment_bytes(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stored_name = format!("{}_{}", &uuid::Uuid::new_v4().to_string()[..8], name);
    let dest = dir.join(stored_name);
    std::fs::write(&dest, bytes)?;
    Ok(dest)
}

/// Copy an existing file into the attachment dir with the same collision-safe
/// `<uuid8>_<name>` naming. Uses `fs::copy` (kernel-level, reflink-capable on
/// CoW filesystems) rather than read-into-RAM-then-write, so a large file
/// (scope capture, video) never gets buffered whole in memory.
fn copy_attachment_file(dir: &Path, name: &str, source: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stored_name = format!("{}_{}", &uuid::Uuid::new_v4().to_string()[..8], name);
    let dest = dir.join(stored_name);
    std::fs::copy(source, &dest)?;
    Ok(dest)
}

/// Convert protocol Measurement to internal Measurement.
/// Returns None if value cannot be parsed.
fn try_measurement_from_protocol(
    m: protocol::Measurement,
) -> Option<crate::measurements::Measurement> {
    let value = serde_json::from_value(m.value).ok()?;
    let aggregations = m.aggregations.and_then(|v| serde_json::from_value(v).ok());
    Some(crate::measurements::Measurement {
        name: m.name,
        value,
        unit: m.unit,
        timestamp: m.timestamp,
        validators: None,
        aggregations,
        description: None,
        // Finalized by `evaluate_measurements` at phase complete.
        outcome: crate::procedure::schema::ValidatorOutcome::Unset,
    })
}

/// Where a worker is in its spawn. `Starting` is the background spawn
/// kicked by `start_in_background`; a job dispatched meanwhile waits
/// for it in `execute_python_phase` instead of failing with "not
/// started", so the pool spawn overlaps identify and plug startup.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerStart {
    NotStarted,
    Starting,
    Ready,
    Failed(String),
}

/// Upper bound on a background spawn, from `await_started`'s point of
/// view: the child's port handshake has its own 90s cap inside
/// `ChildProcess::spawn`, so only a spawn task that never settles
/// (panic, runtime teardown) reaches this.
const WORKER_START_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Settles the start state if the spawn task ends without reporting:
/// a panic inside the spawn, or the task dropped at runtime shutdown,
/// would otherwise leave `Starting` forever.
struct SettleOnDrop(Arc<tokio::sync::watch::Sender<WorkerStart>>);
impl Drop for SettleOnDrop {
    fn drop(&mut self) {
        if *self.0.borrow() == WorkerStart::Starting {
            self.0.send_replace(WorkerStart::Failed(
                "worker spawn task ended without an outcome".to_string(),
            ));
        }
    }
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub id: usize,
    inner: Arc<RwLock<Option<ChildProcess>>>,
    /// Spawn state, shared with the background start task. Receivers
    /// (`subscribe`) are how a dispatched job waits for readiness.
    start: Arc<tokio::sync::watch::Sender<WorkerStart>>,
    procedure_dir: PathBuf,
    /// Pre-resolved Python interpreter. When set, `start()` skips the
    /// `resolve_python` walk-up and uses this path directly. CLI runs
    /// always set this (deterministic `<package_dir>/venv/python`); legacy
    /// callers (Studio, tests) leave it `None` and keep the walk-up.
    python_path: Option<PathBuf>,
    /// When `Some(port)`, the worker subprocess gets TP_DEBUG +
    /// TP_DEBUG_PORT so tp_worker.py starts the debugpy listener. Only
    /// set on pool workers; respawn and teardown workers stay None.
    debug_port: Option<u16>,
    /// The procedure's `dashboard:` credentials, set on the worker
    /// subprocess and on every shell phase it runs. Empty for a
    /// procedure without `dashboard:`. Unlike `debug_port`, every
    /// worker carries it, respawn and teardown workers included: a
    /// phase must find the same environment whichever worker runs it.
    procedure_env: crate::procedure_env::ProcedureEnv,
    /// The shell phase running on this worker. A stop reaches a phase
    /// only through its worker, and a shell phase is not the Python
    /// interpreter that `force_shutdown` kills.
    running_shell: Arc<std::sync::Mutex<RunningShell>>,
    /// Where the interpreter's own stderr lines go.
    stderr_route: Arc<StderrRoute>,
}

#[derive(Debug, Default)]
struct RunningShell {
    group_id: Option<u32>,
    stopped: bool,
}

struct RunningShellGuard<'a>(&'a std::sync::Mutex<RunningShell>);
impl Drop for RunningShellGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = RunningShell::default();
    }
}

/// Lines kept for the error of a phase whose worker died under it.
const STDERR_TAIL_LINES: usize = 20;

/// The interpreter's stderr outside a phase's own capture: C-level
/// writes to fd 2 (faulthandler's "Fatal Python error", a vendor
/// library) and the worker's internal errors. They used to reach only
/// `log::warn!`, which the CLI never installs a backend for. Now they
/// are log lines of the phase running on the worker, and the last few
/// are added to the error when the worker dies mid-phase.
#[derive(Default)]
struct StderrRoute {
    target: std::sync::Mutex<Option<StderrTarget>>,
    tail: std::sync::Mutex<VecDeque<String>>,
    closed: std::sync::atomic::AtomicBool,
}

struct StderrTarget {
    job_id: String,
    phase_key: String,
    slot_id: Option<String>,
    sink: Arc<dyn EventSink>,
}

impl std::fmt::Debug for StderrRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StderrRoute").finish_non_exhaustive()
    }
}

impl StderrRoute {
    fn begin(&self, job: &Job, sink: &Arc<dyn EventSink>) {
        self.tail.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *self.target.lock().unwrap_or_else(|e| e.into_inner()) = Some(StderrTarget {
            job_id: job.id.to_string(),
            phase_key: job.phase_key.clone(),
            slot_id: job.slot_id.clone(),
            sink: Arc::clone(sink),
        });
    }

    fn end(&self) {
        *self.target.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn line(&self, worker_id: usize, line: &str) {
        {
            let mut tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line.to_string());
        }
        let target = self.target.lock().unwrap_or_else(|e| e.into_inner());
        match target.as_ref() {
            Some(t) => t.sink.emit(&ExecutionEvent::PhaseLogLine {
                job_id: t.job_id.clone(),
                phase_key: t.phase_key.clone(),
                slot_id: t.slot_id.clone(),
                level: "WARNING".to_string(),
                message: line.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                file: None,
                line: None,
            }),
            None => log::debug!("Worker {} Python stderr: {}", worker_id, line),
        }
    }

    /// `error` plus what the interpreter printed last. The stderr pipe
    /// can still hold the dying words when the socket closes, so this
    /// waits (briefly) for the pipe to close too.
    async fn with_tail(&self, error: String) -> String {
        for _ in 0..20 {
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        if tail.is_empty() {
            return error;
        }
        let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
        format!(
            "{error}\nWorker stderr (last {} lines):\n{}",
            lines.len(),
            lines.join("\n")
        )
    }
}

/// Clears the stderr target when a Python phase returns, on any path.
struct StderrTargetGuard<'a>(&'a StderrRoute);
impl Drop for StderrTargetGuard<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Startup stage of a dispatched Python job. Bounds each socket read
/// until user code owns the clock (see the loop in
/// `execute_python_phase`). Kept as data, not control flow, so the
/// stage → deadline / stage → error mapping is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStage {
    /// Command written, nothing heard back yet.
    AwaitingAck,
    /// `JobAck` (or any first event) seen; phase module importing.
    Loading,
    /// `ModuleLoaded` seen; the phase body is running.
    Executing,
}

/// Marker substrings embedded in startup-stall errors. The orchestrator's
/// worker-replacement branch matches on these — keep both sides tied to
/// these consts so editing one can't silently detach pool replacement.
pub const STALL_MARKER_ACK: &str = "did not acknowledge";
pub const STALL_MARKER_IMPORT: &str = "stalled while importing";

impl JobStage {
    /// Max silence tolerated on the worker socket in this stage.
    /// `None` = unbounded (phase timeout / operator owns it).
    fn read_deadline(self) -> Option<std::time::Duration> {
        use crate::constants::timeouts::{JOB_ACK_TIMEOUT_SECS, MODULE_IMPORT_TIMEOUT_SECS};
        match self {
            JobStage::AwaitingAck => Some(std::time::Duration::from_secs(JOB_ACK_TIMEOUT_SECS)),
            JobStage::Loading => Some(std::time::Duration::from_secs(MODULE_IMPORT_TIMEOUT_SECS)),
            JobStage::Executing => None,
        }
    }

    /// Actionable operator-facing error for a stall in this stage.
    fn stall_error(self, job: &crate::job::Job) -> String {
        use crate::constants::timeouts::{JOB_ACK_TIMEOUT_SECS, MODULE_IMPORT_TIMEOUT_SECS};
        match self {
            JobStage::AwaitingAck => format!(
                "Python worker {STALL_MARKER_ACK} phase {} within {}s; likely blocked by \
                 antivirus or EDR: allowlist the venv Python",
                job.phase_name, JOB_ACK_TIMEOUT_SECS
            ),
            JobStage::Loading => format!(
                "Phase {} {STALL_MARKER_IMPORT} {} for {}s; move hardware and network \
                 opens into the phase function",
                job.phase_name, job.module, MODULE_IMPORT_TIMEOUT_SECS
            ),
            JobStage::Executing => unreachable!("Executing stage has no read deadline"),
        }
    }
}

/// Outcome of one stage-bounded read off the worker socket.
enum StageRead {
    /// Boxed: an event is ten times the size of the other outcomes.
    Event(Box<protocol::WorkerEvent>),
    Eof,
    Io(String),
    /// The stage's deadline elapsed with total silence.
    Stalled,
}

/// One read off the worker event stream, bounded by the current job
/// stage's deadline (none in debug mode or once `Executing`). Generic
/// over the reader so the stall behavior is testable against an
/// in-memory duplex without a live worker process.
async fn read_stage_event<R>(reader: &mut R, stage: JobStage, debug: bool) -> StageRead
where
    R: tokio::io::AsyncBufRead + Unpin + Send,
{
    let deadline = if debug { None } else { stage.read_deadline() };
    let read = match deadline {
        Some(d) => match tokio::time::timeout(d, transport::read_json_line(reader)).await {
            Ok(r) => r,
            Err(_elapsed) => return StageRead::Stalled,
        },
        None => transport::read_json_line(reader).await,
    };
    match read {
        Ok(Some(event)) => StageRead::Event(Box::new(event)),
        Ok(None) => StageRead::Eof,
        Err(e) => StageRead::Io(e),
    }
}

/// Aborts the job's socket reader when the job loop returns, whichever
/// way it returns.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Read the worker's event stream on a task of its own and hand each read
/// to the job loop with the stage it was read in. The loop has to listen
/// to the socket and to an operator answer at once (`ui.wait()`), and a
/// line read cannot be raced in a `select!`: dropping `read_line`
/// mid-line loses what it had buffered. A channel receive can.
///
/// Stage transitions are a function of the events alone (any event ends
/// `AwaitingAck`, `ModuleLoaded` starts `Executing`), so they live here
/// with the reads they bound. The task ends after the first non-event
/// read (EOF, I/O error, stall) or when the loop stops listening.
fn spawn_event_reader<R>(
    mut reader: R,
    debug: bool,
) -> (
    tokio::sync::mpsc::Receiver<(StageRead, JobStage)>,
    AbortOnDrop,
)
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let handle = tokio::spawn(async move {
        let mut stage = JobStage::AwaitingAck;
        loop {
            let read = read_stage_event(&mut reader, stage, debug).await;
            let read_in = stage;
            let last = !matches!(read, StageRead::Event(_));
            if let StageRead::Event(ref event) = read {
                if stage == JobStage::AwaitingAck {
                    stage = JobStage::Loading;
                }
                if matches!(**event, protocol::WorkerEvent::ModuleLoaded(_)) {
                    stage = JobStage::Executing;
                }
            }
            if tx.send((read, read_in)).await.is_err() || last {
                break;
            }
        }
    });
    (rx, AbortOnDrop(handle))
}

/// Every prompt a Python job registered, unregistered when the job's
/// future is dropped. The orchestrator's phase timeout drops it mid-await
/// (it races the worker's own timeout and kills the worker), which skips
/// the loop's own cleanup and used to leave the prompt's channel in the
/// global map for the rest of the process. Unregistering an id twice is
/// harmless, so the normal paths need not report back here.
#[derive(Default)]
struct RegisteredPrompts(Vec<String>);
impl Drop for RegisteredPrompts {
    fn drop(&mut self) {
        let ids = std::mem::take(&mut self.0);
        if ids.is_empty() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                for id in ids {
                    crate::ui::channels::unregister_ui_channel(&id).await;
                }
            });
        }
    }
}

/// Why a `ui.wait()` is cancelled when its answer leaves a required field
/// empty. Only an unchecked submit (`ui_response::send`) can do that; it
/// is not the operator pressing Esc, so the message must not say so.
const INCOMPLETE_WAIT_ANSWER: &str = "ui.wait(): required field empty in answer";

/// The prompt a blocked `ui.wait()` is answered through.
struct PendingWait {
    wait_id: String,
    request_id: String,
    rx: tokio::sync::oneshot::Receiver<HashMap<String, String>>,
}

/// What the job loop is woken by.
enum LoopInput {
    Read(Option<(StageRead, JobStage)>),
    Answer(Result<HashMap<String, String>, tokio::sync::oneshot::error::RecvError>),
    /// A panel button press (`channels::press`), for `ui.pressed()`.
    Press(String),
}

/// Build the subprocess env for a worker. When `debug_port` is set the
/// worker gets `TP_DEBUG=1` + `TP_DEBUG_PORT`, which makes tp_worker.py
/// start a debugpy listener. Pure so the env contract is unit-testable.
fn build_worker_env(
    worker_id: usize,
    debug_port: Option<u16>,
    procedure_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env_vars = vec![("WORKER_ID".to_string(), worker_id.to_string())];
    if let Some(port) = debug_port {
        env_vars.push(("TP_DEBUG".to_string(), "1".to_string()));
        env_vars.push(("TP_DEBUG_PORT".to_string(), port.to_string()));
    }
    env_vars.extend(procedure_env.iter().cloned());
    env_vars
}

/// An answer written back to a `ui.wait()`: the values and the
/// components they were given on.
type WaitAnswer = (HashMap<String, String>, Vec<crate::ui::UiComponent>);

impl Worker {
    pub fn new(id: usize, procedure_dir: PathBuf) -> Self {
        Self::new_with_python(id, procedure_dir, None)
    }

    pub fn new_with_python(
        id: usize,
        procedure_dir: PathBuf,
        python_path: Option<PathBuf>,
    ) -> Self {
        Self {
            id,
            inner: Arc::new(RwLock::new(None)),
            start: Arc::new(tokio::sync::watch::channel(WorkerStart::NotStarted).0),
            procedure_dir,
            python_path,
            debug_port: None,
            procedure_env: Vec::new(),
            running_shell: Arc::default(),
            stderr_route: Arc::default(),
        }
    }

    /// Builder: mark this worker as a debug worker (adds TP_DEBUG +
    /// TP_DEBUG_PORT to the subprocess env at spawn). `None` = no debug.
    pub fn with_debug_port(mut self, debug_port: Option<u16>) -> Self {
        self.debug_port = debug_port;
        self
    }

    /// Builder: the procedure's `dashboard:` credentials for the
    /// subprocess env at spawn.
    pub fn with_procedure_env(mut self, env: crate::procedure_env::ProcedureEnv) -> Self {
        self.procedure_env = env;
        self
    }

    pub fn set_procedure_env(&mut self, env: crate::procedure_env::ProcedureEnv) {
        self.procedure_env = env;
    }

    /// Bundled worker script, embedded at compile time.
    const WORKER_SCRIPT: &'static str = include_str!("../../python/tp_worker.py");

    pub(crate) fn find_worker_script_cli() -> Result<PathBuf, String> {
        // First check next to the executable (Studio/packaged layout).
        if let Some(p) = super::embedded_script::next_to_exe("tp_worker.py") {
            return Ok(p);
        }
        // Otherwise extract a content-addressed copy to a per-user dir. Writing to
        // `%TEMP%` triggers Windows Defender ASR rules and several
        // enterprise EDR policies, surfacing as `os error 5` on spawn.
        super::embedded_script::extract_to_runtime_dir("tp_worker.py", Self::WORKER_SCRIPT)
    }

    pub async fn start(&mut self, event_sink: &Arc<dyn EventSink>) -> Result<(), String> {
        let python_cmd =
            crate::python::resolve_or_walk(&self.python_path, &self.procedure_dir).await?;
        self.start_with_python(event_sink, &python_cmd).await
    }

    pub async fn start_with_python(
        &mut self,
        _event_sink: &Arc<dyn EventSink>,
        python_cmd: &str,
    ) -> Result<(), String> {
        self.start.send_replace(WorkerStart::Starting);
        match Self::spawn_process(
            self.id,
            &self.procedure_dir,
            python_cmd,
            self.debug_port,
            &self.procedure_env,
            Arc::clone(&self.stderr_route),
        )
        .await
        {
            Ok(process) => {
                *self.inner.write().await = Some(process);
                self.start.send_replace(WorkerStart::Ready);
                Ok(())
            }
            Err(e) => {
                self.start.send_replace(WorkerStart::Failed(e.clone()));
                Err(e)
            }
        }
    }

    /// `start_with_python` without waiting for the interpreter: the
    /// spawn runs on its own task and the first job dispatched to this
    /// worker waits for it (`await_started`). Lets the pool come up
    /// while the run identifies its unit and starts its plugs instead
    /// of before them. A spawn failure surfaces on that first job.
    pub fn start_in_background(&self, python_cmd: String) {
        self.start.send_replace(WorkerStart::Starting);
        let id = self.id;
        let procedure_dir = self.procedure_dir.clone();
        let debug_port = self.debug_port;
        let procedure_env = self.procedure_env.clone();
        let inner = Arc::clone(&self.inner);
        let start = Arc::clone(&self.start);
        let stderr_route = Arc::clone(&self.stderr_route);
        tokio::spawn(async move {
            let _settle = SettleOnDrop(Arc::clone(&start));
            match Self::spawn_process(
                id,
                &procedure_dir,
                &python_cmd,
                debug_port,
                &procedure_env,
                stderr_route,
            )
            .await
            {
                Ok(process) => {
                    *inner.write().await = Some(process);
                    start.send_replace(WorkerStart::Ready);
                }
                Err(e) => {
                    start.send_replace(WorkerStart::Failed(e));
                }
            }
        });
    }

    /// Resolve once the spawn has an outcome. Immediate for a worker
    /// started synchronously; errors for one never started, so the old
    /// "Worker not started" contract holds for callers that skip
    /// `initialize()`.
    pub async fn await_started(&self) -> Result<(), String> {
        let mut rx = self.start.subscribe();
        let state = tokio::time::timeout(
            WORKER_START_DEADLINE,
            rx.wait_for(|s| !matches!(s, WorkerStart::Starting)),
        )
        .await
        .map_err(|_| {
            format!(
                "Python worker {}: not ready after {}s",
                self.id,
                WORKER_START_DEADLINE.as_secs()
            )
        })?
        .map_err(|_| "Worker start task dropped".to_string())?
        .clone();
        match state {
            WorkerStart::Ready => Ok(()),
            WorkerStart::NotStarted => Err("Worker not started".to_string()),
            WorkerStart::Failed(e) => Err(format!("Python worker {}: {}", self.id, e)),
            WorkerStart::Starting => unreachable!("wait_for excludes Starting"),
        }
    }

    /// `await_started` for the scheduler: a worker that was never
    /// started (the respawn path's placeholder) passes through and
    /// fails on dispatch exactly as before, so only a background spawn
    /// that failed or timed out is reported — those are environment
    /// failures the run must stop on, not phase errors.
    pub async fn await_background_start(&self) -> Result<(), String> {
        if *self.start.borrow() == WorkerStart::NotStarted {
            return Ok(());
        }
        self.await_started().await
    }

    /// A shutdown racing a background spawn must not take `inner`
    /// while it is still `None` and then let the child land after: wait
    /// (bounded) for the spawn to settle first. On timeout the child, if
    /// it ever lands, is reaped with the `ChildProcess` on drop.
    async fn settle_start(&self) {
        if *self.start.borrow() != WorkerStart::Starting {
            return;
        }
        let mut rx = self.start.subscribe();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            rx.wait_for(|s| !matches!(s, WorkerStart::Starting)),
        )
        .await;
    }

    async fn spawn_process(
        id: usize,
        procedure_dir: &Path,
        python_cmd: &str,
        debug_port: Option<u16>,
        procedure_env: &[(String, String)],
        stderr_route: Arc<StderrRoute>,
    ) -> Result<ChildProcess, String> {
        // `canonicalize_for_spawn` strips Windows `\\?\` extended-length
        // prefix so `CreateProcessW` accepts the path as a working
        // directory under AV/EDR policies that reject UNC-prefixed cwd.
        let abs_procedure_dir = crate::path_utils::canonicalize_for_spawn(procedure_dir)
            .map_err(|e| format!("Cannot resolve procedure directory: {}", e))?;

        let worker_script = Self::find_worker_script_cli()?;

        log::debug!(
            "Worker {} using NDJSON script: {}",
            id,
            worker_script.display()
        );

        let env_vars = build_worker_env(id, debug_port, procedure_env);

        let process = ChildProcess::spawn(
            python_cmd,
            worker_script,
            vec![
                abs_procedure_dir.to_string_lossy().to_string(),
                // The worker's parent watchdog keys on this pid rather than
                // reading getppid() itself: if this process dies before the
                // child gets there, getppid() already names the reaper and
                // the watchdog never fires. Same contract as tp_plug.py.
                "--parent-pid".to_string(),
                std::process::id().to_string(),
            ],
            Some(&abs_procedure_dir),
            env_vars,
            Some(Box::new(move |stderr| {
                Self::spawn_stderr_reader_static(id, stderr, stderr_route);
            })),
        )
        .await?;

        log::debug!("Worker {} TCP port: {}", id, process.port);
        Ok(process)
    }

    pub async fn execute_python_phase(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        event_sink: Arc<dyn EventSink>,
        attachment_dir: Option<PathBuf>,
    ) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();

        // Pool workers spawn in the background (`start_in_background`);
        // the first job pays whatever is left of that spawn here.
        self.await_started().await?;

        // Emit UI request if phase has components or a panel: a panel
        // that only shows values and buttons is a screen too.
        let has_ui = !job.ui_config.components.is_empty() || job.ui_config.panel.is_some();
        // Panel button presses of every screen this job shows.
        let press_names = job
            .ui_config
            .panel
            .as_ref()
            .map(|p| p.events.clone())
            .unwrap_or_default();
        let (press_tx, mut press_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let requires_user_input = job.ui_config.requires_user_input();

        // The YAML screen's prompt. Answered by the operator's Continue
        // once the Python returns, or consumed by the phase's first
        // `ui.wait()` (see `PendingWait`).
        let mut registered = RegisteredPrompts::default();
        let mut initial_prompt = if has_ui {
            open_screen(
                &job,
                &mut registered,
                &press_names,
                &press_tx,
                event_sink.as_ref(),
                requires_user_input,
            )
            .await
        } else {
            None
        };

        // The screen as the operator currently sees it. Starts as the
        // YAML components and follows every `ui.set` the phase makes, so
        // the submit-time affix composition (`extract_bound_measurements`)
        // records `prefix + input + suffix` with the run-time affixes, not
        // the YAML ones. The pending prompt's copy (`PendingUi`) is
        // patched in the same step, under its lock.
        let mut live_request_id: Option<String> = initial_prompt.as_ref().map(|(id, _)| id.clone());
        let mut live_components = job.ui_config.components.clone();

        // `ui.wait()` state. `next_screen` is the screen a wait re-arms:
        // it takes every `ui.set`, including those made after an answer,
        // which `live_components` must not (the answer's bound affixes are
        // composed with what the operator saw). `live_values` holds what
        // the form should show: the operator's last answer, overwritten by
        // any `ui.<key> = …` written since. `last_answer` feeds `bind:` at
        // the end of the phase (the last submit wins, by decision), with
        // the components it was given on. `wait_cancel` is the reason a
        // wait could not be answered, used like a closed prompt's reason
        // when the phase still ends well.
        let mut next_screen = job.ui_config.components.clone();
        let mut live_values = super::ui_wait::FormValues::new();
        // The panel's own values (`ui.<key> = v`, no component), sent
        // again after each screen a wait re-arms: that screen starts
        // from the components alone.
        let mut panel_values: std::collections::BTreeMap<String, serde_json::Value> =
            std::collections::BTreeMap::new();
        let mut pending_wait: Option<PendingWait> = None;
        let mut waited = false;
        // The last two answers written back, by wait id: `bind:` records
        // the one the phase says it received (`ui_answered_wait_id`), which
        // is the newest unless that one arrived after the phase ended.
        let mut answers: Vec<(String, WaitAnswer)> = Vec::new();
        let mut wait_cancel: Option<String> = None;
        // The cancelled wait was closed by a stop, not because nobody could
        // answer it: the phase did not ask for the stop it then reads.
        let mut wait_cut_by_stop = false;

        let command = job_command(&job, plug_ports, press_names.clone());

        // Connect to worker TCP and send command
        let port = {
            let inner = self.inner.read().await;
            inner.as_ref().ok_or("Worker not started")?.port
        };

        let stream = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .map_err(|e| format!("Python worker unreachable: {}", e))?;

        let (read_half, mut write_half) = tokio::io::split(stream);
        let reader = BufReader::new(read_half);

        self.stderr_route.begin(&job, &event_sink);
        let _stderr_target = StderrTargetGuard(&self.stderr_route);

        // Send command as JSON line
        transport::write_json_line(&mut write_half, &command).await?;

        // Startup-stage deadlines. The Python-side phase timeout is
        // enforced from inside the worker interpreter, so it cannot fire
        // when that interpreter is wedged before the phase body runs — a
        // suspended process (EDR) or a blocked module-level import used
        // to hang the run forever with no diagnostic. Each read is
        // bounded by the current stage until user code owns the clock:
        //   AwaitingAck  — command sent, nothing heard: pure liveness.
        //   Loading      — acked, module importing: any streamed event
        //                  (log line, etc.) resets the window.
        //   Executing    — `ModuleLoaded` seen: no Rust-side deadline;
        //                  the phase timeout / operator owns it.
        // Debug workers skip all deadlines — a breakpoint pause is
        // indistinguishable from a stall. The reads and the stage live in
        // `spawn_event_reader`.
        let (mut events, _reader_task) = spawn_event_reader(reader, self.debug_port.is_some());

        // Read streaming events until EOF, and an operator answer while a
        // `ui.wait()` is blocked: the phase timeout and the live updates
        // of other Python threads still arrive on the socket meanwhile.
        loop {
            // Unbiased on purpose: a phase streaming updates must not
            // starve the answer. A late answer is told apart by
            // `ui_answered_wait_id` on JobComplete instead.
            let answer = async {
                match pending_wait.as_mut() {
                    Some(wait) => (&mut wait.rx).await,
                    None => std::future::pending().await,
                }
            };
            let input = tokio::select! {
                read = events.recv() => LoopInput::Read(read),
                answer = answer => LoopInput::Answer(answer),
                Some(name) = press_rx.recv() => LoopInput::Press(name),
            };
            let read = match input {
                LoopInput::Read(read) => read,
                LoopInput::Press(name) => {
                    let message = protocol::EngineMessage::UiEvent { name };
                    if let Err(e) = transport::write_json_line(&mut write_half, &message).await {
                        log::debug!("Phase {}: could not deliver a press: {}", job.phase_name, e);
                    }
                    continue;
                }
                LoopInput::Answer(answer) => {
                    let wait = pending_wait.take().expect("selected on it");
                    crate::ui::channels::unregister_ui_channel(&wait.request_id).await;
                    // Every operator and agent submit is validated before it
                    // resolves the prompt; only an unchecked one
                    // (`ui_response::send`) can get here incomplete. A phase
                    // must never act on an answer the operator did not give,
                    // so that is a cancel, not `None` in a required field.
                    let answer = answer.map_err(|_| None).and_then(|values| {
                        if crate::ui::validate_response(&live_components, &values).is_empty() {
                            Ok(values)
                        } else {
                            Err(Some(INCOMPLETE_WAIT_ANSWER.to_string()))
                        }
                    });
                    let reply = match answer {
                        Ok(values) => {
                            log::debug!(
                                "ui.wait {} of phase {} answered",
                                wait.wait_id,
                                job.phase_name
                            );
                            let typed = super::ui_wait::typed_answer(&live_components, &values);
                            super::ui_wait::answer_as_form_values(
                                &live_components,
                                &values,
                                &mut live_values,
                            );
                            if answers.len() == 2 {
                                answers.remove(0);
                            }
                            answers.push((wait.wait_id.clone(), (values, live_components.clone())));
                            // The phase got its answer after all (it caught
                            // an earlier cancel and waited again).
                            wait_cancel = None;
                            protocol::EngineMessage::UiAnswer {
                                wait_id: wait.wait_id,
                                values: typed,
                            }
                        }
                        Err(known) => {
                            wait_cut_by_stop = known.is_none()
                                && !crate::ui::channels::is_unanswerable(&wait.request_id).await;
                            let reason = match known {
                                Some(reason) => reason,
                                None => crate::ui::channels::take_cancel_reason(&wait.request_id)
                                    .await
                                    .unwrap_or_else(|| {
                                        "Operator prompt closed unanswered".to_string()
                                    }),
                            };
                            log::debug!(
                                "ui.wait {} of phase {} cancelled: {}",
                                wait.wait_id,
                                job.phase_name,
                                reason
                            );
                            wait_cancel = Some(reason.clone());
                            protocol::EngineMessage::UiCancel {
                                wait_id: wait.wait_id,
                                reason,
                            }
                        }
                    };
                    // A worker that already went away shows up as EOF on
                    // the read side; nothing to do about it here.
                    if let Err(e) = transport::write_json_line(&mut write_half, &reply).await {
                        log::debug!("Phase {}: could not answer ui.wait: {}", job.phase_name, e);
                    }
                    continue;
                }
            };
            let event = match read {
                Some((StageRead::Event(e), _)) => *e,
                None | Some((StageRead::Eof, _)) => break,
                Some((StageRead::Io(e), _)) => return Err(self.stderr_route.with_tail(e).await),
                Some((StageRead::Stalled, stage)) => {
                    // Alive-but-silent worker. Kill it so it can't
                    // linger wedged behind the pool, then fail the
                    // job with a stage-specific, actionable error —
                    // this propagates as a phase ERROR and turns the
                    // former infinite spinner into a diagnosis.
                    self.kill_wedged().await;
                    return Err(stage.stall_error(&job));
                }
            };

            match event {
                protocol::WorkerEvent::JobAck(_) => {
                    // The reader advanced the stage. Surface the transition
                    // as a phase log so the operator UI's Advanced tab
                    // shows startup progress instead of dead air — the
                    // exact signal Sander's stalled run was missing.
                    self.emit_stage_log(
                        &job,
                        &event_sink,
                        "Python worker acknowledged phase, importing module...",
                    );
                }
                protocol::WorkerEvent::ModuleLoaded(_) => {
                    self.emit_stage_log(
                        &job,
                        &event_sink,
                        "Module imported, running phase function",
                    );
                }
                protocol::WorkerEvent::JobComplete(result) => {
                    // Check phase result to determine if we should wait for UI
                    let phase_result = result
                        .phase_result_json
                        .as_ref()
                        .and_then(|json| serde_json::from_str(json).ok())
                        .and_then(|pr: crate::ui::PythonPhaseResult| {
                            crate::job::PhaseResult::from_python_result(&pr).ok()
                        })
                        .unwrap_or(crate::job::PhaseResult::Continue);

                    let is_terminal = matches!(
                        phase_result,
                        crate::job::PhaseResult::Skip
                            | crate::job::PhaseResult::Stop
                            | crate::job::PhaseResult::Fail
                    ) || result.error.is_some();

                    let mut ui_unit_info: Option<crate::unit::UnitInfo> = None;
                    let mut ui_bound_measurements: Option<HashMap<String, serde_json::Value>> =
                        None;
                    // True when the UI was required but we never got a value.
                    // Forces the phase to ERROR so a silent timeout can't pass.
                    let mut ui_missing_required = false;
                    // A prompt or wait of the phase closed unanswered (see
                    // `JobResult::prompt_closed`).
                    let mut prompt_closed = false;
                    // Why, when the cause is already known here (a
                    // cancelled `ui.wait()`); otherwise looked up below.
                    let mut ui_missing_reason: Option<String> = None;
                    let ui_request_id: Option<String> =
                        initial_prompt.as_ref().map(|(id, _)| id.clone());
                    if waited {
                        // The phase talked to its operator through
                        // `ui.wait()`: the screen was consumed, there is no
                        // final Continue to wait for. A wait still open
                        // here was cut short (phase timeout, an error in
                        // another thread): close its prompt; `phase_complete`
                        // clears it on every surface.
                        //
                        // A phase that returns normally without an answer
                        // it waited for (a wait still open on a thread of
                        // its own, a cancel it caught) must not pass. One
                        // that ended on its own terms (`phase.skip()`,
                        // `fail()`, `stop()`, an error) keeps its outcome
                        // and its message: it handled the missing answer.
                        // `phase_outcome` is still a placeholder here, so
                        // this test is the only guard.
                        let unanswered_pass = !is_terminal;
                        if let Some(wait) = pending_wait.take() {
                            crate::ui::channels::unregister_ui_channel(&wait.request_id).await;
                            if unanswered_pass {
                                ui_missing_required = true;
                                ui_missing_reason =
                                    Some("Phase ended during ui.wait()".to_string());
                            }
                        }
                        let received = result.ui_answered_wait_id.as_ref().and_then(|id| {
                            answers
                                .iter()
                                .position(|(wait_id, _)| wait_id == id)
                                .map(|at| answers.swap_remove(at).1)
                        });
                        if let Some((values, components)) = received {
                            if let Some((unit_info, bound)) =
                                extract_bound_measurements(&values, &components)
                            {
                                ui_unit_info = unit_info;
                                ui_bound_measurements = Some(bound);
                            }
                        }
                        if let Some(reason) = wait_cancel.take() {
                            // Raised in the phase as `WaitCancelled`, or
                            // passed over: the wait was closed under it.
                            prompt_closed = wait_cut_by_stop;
                            if unanswered_pass {
                                ui_missing_required = true;
                                ui_missing_reason = Some(reason);
                            }
                        }
                    } else if let Some((request_id, mut rx)) = initial_prompt.take() {
                        match rx.try_recv() {
                            Ok(ui_values) => {
                                log::debug!("UI already submitted for phase {}", job.phase_name);
                                crate::ui::channels::unregister_ui_channel(&request_id).await;
                                if let Some((unit_info, bound)) =
                                    extract_bound_measurements(&ui_values, &live_components)
                                {
                                    ui_unit_info = unit_info;
                                    ui_bound_measurements = Some(bound);
                                }
                            }
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                if is_terminal {
                                    log::debug!(
                                        "Python phase {} returned terminal result {:?}, dismissing UI",
                                        job.phase_name, phase_result
                                    );
                                    drop(rx);
                                    crate::ui::channels::unregister_ui_channel(&request_id).await;
                                } else {
                                    log::debug!(
                                        "Python phase {} finished, waiting for UI submission",
                                        job.phase_name
                                    );
                                    match rx.await {
                                        Ok(ui_values) => {
                                            log::debug!(
                                                "Received UI submission for phase {}",
                                                job.phase_name
                                            );
                                            crate::ui::channels::unregister_ui_channel(&request_id)
                                                .await;
                                            if let Some((unit_info, bound)) =
                                                extract_bound_measurements(
                                                    &ui_values,
                                                    &live_components,
                                                )
                                            {
                                                ui_unit_info = unit_info;
                                                ui_bound_measurements = Some(bound);
                                            }
                                        }
                                        Err(_) => {
                                            log::debug!(
                                                "UI response channel closed for phase {}",
                                                job.phase_name
                                            );
                                            ui_missing_required = true;
                                            prompt_closed =
                                                !crate::ui::channels::is_unanswerable(&request_id)
                                                    .await;
                                            crate::ui::channels::unregister_ui_channel(&request_id)
                                                .await;
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                                log::debug!(
                                    "UI response channel closed for phase {}",
                                    job.phase_name
                                );
                                ui_missing_required = true;
                                prompt_closed =
                                    !crate::ui::channels::is_unanswerable(&request_id).await;
                                crate::ui::channels::unregister_ui_channel(&request_id).await;
                            }
                        }
                    }

                    let end_time = chrono::Utc::now();

                    let mut job_result = convert_job_result(result, start_time, end_time, &job)?;
                    job_result.prompt_closed = prompt_closed;

                    // A required UI input that was never answered (channel
                    // closed) must fail the phase — the framework can't trust
                    // the body's PASS result if the operator never confirmed.
                    // Don't clobber a pre-existing Error/Fail/Timeout diagnostic
                    // from the phase body (e.g. "measurement X failed"); only
                    // override outcomes that would otherwise look successful.
                    if ui_missing_required
                        && matches!(
                            job_result.phase_outcome,
                            crate::job::Outcome::Pass | crate::job::Outcome::Retry
                        )
                    {
                        job_result.phase_outcome = crate::job::Outcome::Error;
                        // Surface the orchestrator's cancel reason when set
                        // (e.g. "Run aborted by phase 'X': <traceback>") so
                        // the operator sees the *cause* of the cancellation
                        // instead of a generic "cancelled or timed out".
                        // Falls back to the generic message when the channel
                        // closed for an unrelated reason (operator timeout,
                        // direct cancel from the agent stdin reader, etc.).
                        let reason = match (ui_missing_reason, &ui_request_id) {
                            (Some(r), _) => Some(r),
                            (None, Some(id)) => crate::ui::channels::take_cancel_reason(id).await,
                            (None, None) => crate::ui::channels::CANCEL_REASON.read().await.clone(),
                        };
                        job_result.error = Some(match reason {
                            Some(r) => r,
                            None => "Operator prompt closed unanswered".to_string(),
                        });
                    }

                    finalize_and_announce(
                        &mut job_result,
                        ui_bound_measurements,
                        &job,
                        event_sink.as_ref(),
                    );

                    if let Some(ui_unit) = ui_unit_info {
                        let merged = merge_unit_info(job_result.unit, ui_unit);
                        // Mid-run unit update: re-publish the resolved
                        // unit through the same event used pre-run so
                        // every consumer (operator-UI, dashboard,
                        // agent stream) updates its `RunState.unit`
                        // field-level. Without this the dashboard
                        // sees the new unit at upload time but the
                        // operator-UI's running screen stays stale.
                        event_sink.emit(&ExecutionEvent::UnitIdentified {
                            slot_id: job.slot_id.clone(),
                            unit_info: merged.clone(),
                        });
                        job_result.unit = Some(merged);
                    } else if let Some(py_unit) = job_result.unit.as_ref() {
                        // Pure Python write (`unit.<field> = ...` with no
                        // UI response this phase): publish through the
                        // same event, but only when something actually
                        // changed — the worker echoes the seeded unit
                        // back on every phase, and re-emitting identical
                        // identity each phase would spam consumers.
                        if unit_identity_changed(py_unit, job.initial_unit_info.as_ref()) {
                            event_sink.emit(&ExecutionEvent::UnitIdentified {
                                slot_id: job.slot_id.clone(),
                                unit_info: py_unit.clone(),
                            });
                        }
                    }

                    return Ok(job_result);
                }
                protocol::WorkerEvent::Error(err) => {
                    return Err(err.message);
                }
                protocol::WorkerEvent::AttachFile(attach_event) => {
                    let source_path = attach_event.source_path.clone();
                    let attachment_name = attach_event.attachment_name.clone();

                    // Copy the source into the caller-owned attachment dir so
                    // the emitted path points at a file the CLI controls and
                    // deletes after upload. Without a dir (legacy callers),
                    // emit the original source path unchanged.
                    let emitted_path = match attachment_dir {
                        Some(ref dir) => {
                            match copy_attachment_file(dir, &attachment_name, &source_path) {
                                Ok(dest) => Some(dest.to_string_lossy().into_owned()),
                                Err(e) => {
                                    log::warn!("Cannot store attachment {attachment_name}: {e}");
                                    Some(source_path.clone())
                                }
                            }
                        }
                        None => Some(source_path.clone()),
                    };

                    event_sink.emit(&ExecutionEvent::AttachmentAdded {
                        phase_key: job.phase_key.clone(),
                        slot_id: job.slot_id.clone(),
                        name: attachment_name.clone(),
                        path: emitted_path,
                        // Guess from the filename so the operator-UI can tell
                        // images apart (it gates `<img>` rendering on an
                        // `image/*` mimetype).
                        mimetype: guess_attachment_mimetype(&attachment_name),
                    });
                }
                protocol::WorkerEvent::AttachData(attach_event) => {
                    // Write the bytes to the attachment dir FIRST so the live
                    // event can carry the stored on-disk path — the kiosk
                    // serves attachment images from it (`/attachments/*`)
                    // until the upload queue deletes the file. Emitting
                    // before the write would force `path: None` and leave
                    // the kiosk unable to render `attach.data` images.
                    let mut stored_path: Option<String> = None;
                    if let Some(ref dir) = attachment_dir {
                        match base64::engine::general_purpose::STANDARD.decode(&attach_event.data) {
                            Ok(bytes) => {
                                match store_attachment_bytes(
                                    dir,
                                    &attach_event.attachment_name,
                                    &bytes,
                                ) {
                                    Ok(dest) => {
                                        stored_path = Some(dest.to_string_lossy().into_owned());
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Cannot store attachment {}: {}",
                                            attach_event.attachment_name,
                                            e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Cannot store attachment {}: invalid base64: {}",
                                    attach_event.attachment_name,
                                    e
                                );
                            }
                        }
                    }
                    // Emit after the write so `path` points at the stored
                    // file (None if no attachment dir was active or the decode
                    // failed — the remote UI still resolves via upload id).
                    event_sink.emit(&ExecutionEvent::AttachmentAdded {
                        phase_key: job.phase_key.clone(),
                        slot_id: job.slot_id.clone(),
                        name: attach_event.attachment_name.clone(),
                        path: stored_path,
                        mimetype: guess_attachment_mimetype(&attach_event.attachment_name),
                    });
                }
                protocol::WorkerEvent::UiWait(wait) => {
                    let refusal = if pending_wait.is_some() {
                        // `wait()` blocks its thread; two at once means two
                        // threads of one phase waiting on one screen.
                        Some("already pending in this phase".to_string())
                    } else if !has_ui {
                        Some(format!(
                            "phase {} has no `ui.components` or `ui.panel` in procedure.yaml",
                            job.phase_key
                        ))
                    } else {
                        None
                    };
                    if let Some(reason) = refusal {
                        let reply = protocol::EngineMessage::UiRefused {
                            wait_id: wait.wait_id,
                            reason,
                        };
                        if let Err(e) = transport::write_json_line(&mut write_half, &reply).await {
                            log::debug!(
                                "Phase {}: could not refuse ui.wait: {}",
                                job.phase_name,
                                e
                            );
                        }
                        continue;
                    }
                    waited = true;
                    if let Some((request_id, rx)) = initial_prompt.take() {
                        // The first wait answers the screen already up,
                        // whether or not the operator has submitted it yet.
                        pending_wait = Some(PendingWait {
                            wait_id: wait.wait_id,
                            request_id,
                            rx,
                        });
                        continue;
                    }
                    // Re-arm: the same screen as it is now (every `ui.set`
                    // applied) with its latest values, behind a Continue
                    // even when it only displays (a display-only screen is
                    // otherwise auto-continued by headless runs).
                    let request_id = format!(
                        "{}_{}_w{}",
                        job.id,
                        chrono::Utc::now().timestamp_millis(),
                        wait.wait_id
                    );
                    let components =
                        super::ui_wait::components_for_rearm(&next_screen, &live_values);
                    live_components = next_screen.clone();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    registered.0.push(request_id.clone());
                    crate::ui::channels::register_ui_channel(
                        request_id.clone(),
                        job.slot_id.clone(),
                        job.is_teardown(),
                        tx,
                        components.clone(),
                    )
                    .await;
                    crate::ui::channels::register_press_sink(
                        &request_id,
                        press_names.clone(),
                        press_tx.clone(),
                    );
                    event_sink.emit(&ExecutionEvent::UiRequest(crate::ui::UiRequestData {
                        request_id: request_id.clone(),
                        job_id: job.id.to_string(),
                        pipe_path: String::new(),
                        config: crate::ui::UiConfig {
                            components,
                            requires_input: Some(true),
                            panel: job.ui_config.panel.clone(),
                        },
                        phase_key: job.phase_key.clone(),
                        slot_id: job.slot_id.clone(),
                    }));
                    for (key, value) in &panel_values {
                        event_sink.emit(&ExecutionEvent::UiUpdate(crate::events::UiUpdateEvent {
                            job_id: job.id.to_string(),
                            slot_id: job.slot_id.as_deref().unwrap_or("<shared>").to_string(),
                            phase_key: job.phase_key.clone(),
                            worker_id: self.id,
                            action: "set_value".to_string(),
                            data: serde_json::json!({ "id": key, "value": value }),
                        }));
                    }
                    log::debug!(
                        "Sent UI request {} for ui.wait {} of phase {}",
                        request_id,
                        wait.wait_id,
                        job.phase_name
                    );
                    live_request_id = Some(request_id.clone());
                    pending_wait = Some(PendingWait {
                        wait_id: wait.wait_id,
                        request_id,
                        rx,
                    });
                }
                protocol::WorkerEvent::UiUpdate(ui_event) => {
                    if ui_event.action == "set_value" {
                        // Held for the screen a later `ui.wait()` re-arms;
                        // the update itself still goes out below.
                        if let Ok(payload) =
                            serde_json::from_str::<serde_json::Value>(&ui_event.data_json)
                        {
                            // A key no component declares is a panel's
                            // display value: shown, never part of a form.
                            let id = payload.get("id").and_then(|v| v.as_str());
                            if let Some(key) =
                                id.filter(|k| !job.ui_config.components.iter().any(|c| c.key == *k))
                            {
                                match payload.get("value") {
                                    None | Some(serde_json::Value::Null) => {
                                        panel_values.remove(key);
                                    }
                                    Some(v) => {
                                        panel_values.insert(key.to_string(), v.clone());
                                    }
                                }
                            } else if let Some(key) = id {
                                let value =
                                    payload.get("value").unwrap_or(&serde_json::Value::Null);
                                match super::ui_wait::component_value_from_json(value) {
                                    Some(v) => {
                                        live_values.insert(key.to_string(), Some(v));
                                    }
                                    // `ui.<key> = None` clears it.
                                    None if value.is_null() => {
                                        live_values.insert(key.to_string(), None);
                                    }
                                    None => {}
                                }
                            }
                        }
                    }
                    let data = if ui_event.action == station_protocol::SET_PROPERTIES_ACTION {
                        let payload = match parse_set_properties(&ui_event.data_json) {
                            Ok(payload) => payload,
                            Err(refusal) => {
                                self.emit_ui_set_refusal(&job, &event_sink, &refusal);
                                continue;
                            }
                        };
                        let for_next_screen =
                            patch_component(&mut next_screen, &payload).map(|_| ());
                        // A property change must land on the components the
                        // submit is validated against before any surface
                        // renders it, or the screen and the check disagree.
                        match apply_parsed_set_properties(
                            live_request_id.as_deref(),
                            payload,
                            &mut live_components,
                        )
                        .await
                        {
                            Ok(Some(payload)) => serde_json::to_value(&payload).unwrap_or_default(),
                            // Prompt already answered: nothing on screen to
                            // update. A refusal still matters to the screen
                            // a later `ui.wait()` shows.
                            Ok(None) => {
                                if let Err(refusal) = for_next_screen {
                                    self.emit_ui_set_refusal(&job, &event_sink, &refusal);
                                }
                                continue;
                            }
                            Err(refusal) => {
                                self.emit_ui_set_refusal(&job, &event_sink, &refusal);
                                continue;
                            }
                        }
                    } else {
                        serde_json::from_str(&ui_event.data_json).unwrap_or_default()
                    };
                    event_sink.emit(&ExecutionEvent::UiUpdate(crate::events::UiUpdateEvent {
                        job_id: job.id.to_string(),
                        slot_id: job.slot_id.as_deref().unwrap_or("<shared>").to_string(),
                        phase_key: job.phase_key.clone(),
                        worker_id: self.id,
                        action: ui_event.action.clone(),
                        data,
                    }));
                }
                protocol::WorkerEvent::PhaseLogLine(line_event) => {
                    event_sink.emit(&ExecutionEvent::PhaseLogLine {
                        job_id: line_event.job_id.clone(),
                        phase_key: job.phase_key.clone(),
                        slot_id: job.slot_id.clone(),
                        level: line_event.level.clone(),
                        message: line_event.message.clone(),
                        timestamp: line_event.timestamp.clone(),
                        file: line_event.file.clone(),
                        line: line_event.line,
                    });
                }
                protocol::WorkerEvent::MeasurementRecorded(m) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&m.value_json).unwrap_or(serde_json::Value::Null);
                    let mut judged = crate::measurements::Measurement {
                        name: m.name.clone(),
                        value: serde_json::from_value(value.clone())
                            .unwrap_or(crate::measurements::MeasurementValue::Null),
                        unit: m.unit.clone(),
                        timestamp: m.timestamp.clone(),
                        validators: None,
                        aggregations: None,
                        description: None,
                        outcome: crate::procedure::schema::ValidatorOutcome::Unset,
                    };
                    crate::measurements::evaluation::judge_recorded(
                        &mut judged,
                        &job.phase_measurements,
                    );
                    event_sink.emit(&ExecutionEvent::MeasurementRecorded {
                        job_id: m.job_id.clone(),
                        phase_key: job.phase_key.clone(),
                        slot_id: job.slot_id.clone(),
                        name: m.name.clone(),
                        value,
                        unit: judged.unit,
                        timestamp: m.timestamp.clone(),
                        outcome: judged.outcome,
                        validators: judged.validators.unwrap_or_default(),
                    });
                }
            }
        }

        Err(self
            .stderr_route
            .with_tail("Python worker exited mid-phase".to_string())
            .await)
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        self.kill_running_shell();
        self.settle_start().await;
        let mut inner = self.inner.write().await;

        if let Some(ref mut process) = *inner {
            let result = process.graceful_shutdown_signal(5).await;
            inner.take();
            result
        } else {
            Ok(())
        }
    }

    pub async fn force_shutdown(&mut self) -> Result<(), String> {
        self.kill_running_shell();
        self.settle_start().await;
        let mut inner = self.inner.write().await;

        if let Some(ref mut process) = *inner {
            let result = process.force_kill().await;
            inner.take();
            result
        } else {
            Ok(())
        }
    }

    pub async fn interrupt_current_job(&mut self) -> Result<(), String> {
        self.force_shutdown().await
    }

    /// Whether the interpreter is still up (not shut down or killed).
    /// Alive includes a worker whose background spawn is still in
    /// flight: the hurried stop keeps idle alive workers for the
    /// teardown, and a dispatched teardown job waits for the spawn in
    /// `execute_python_phase` anyway. Counting it dead would kill the
    /// pool and pay a fresh synchronous spawn for the teardown.
    pub async fn is_alive(&self) -> bool {
        self.inner.read().await.is_some() || *self.start.borrow() == WorkerStart::Starting
    }

    /// A `ui.set` the engine refused, as an ERROR line on the phase log:
    /// the author's only trace of it, since the screen stays unchanged.
    fn emit_ui_set_refusal(&self, job: &Job, event_sink: &Arc<dyn EventSink>, refusal: &str) {
        event_sink.emit(&ExecutionEvent::PhaseLogLine {
            job_id: job.id.to_string(),
            phase_key: job.phase_key.clone(),
            slot_id: job.slot_id.clone(),
            level: "ERROR".to_string(),
            message: format!("ui.set: {refusal}"),
            timestamp: chrono::Utc::now().to_rfc3339(),
            file: None,
            line: None,
        });
    }

    /// DEBUG-level startup-stage breadcrumb attached to the running
    /// phase. Streams through the normal `PhaseLogLine` channel so every
    /// consumer (operator-UI Advanced tab, TUI, agent protocol) shows
    /// startup progress without a new event type.
    fn emit_stage_log(&self, job: &Job, event_sink: &Arc<dyn EventSink>, message: &str) {
        event_sink.emit(&ExecutionEvent::PhaseLogLine {
            job_id: job.id.to_string(),
            phase_key: job.phase_key.clone(),
            slot_id: job.slot_id.clone(),
            level: "DEBUG".to_string(),
            message: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            file: None,
            line: None,
        });
    }

    /// Kill a worker whose interpreter is alive but wedged (startup-stage
    /// deadline tripped). `&self` variant of `force_shutdown`: the job
    /// loop only holds a shared ref, and interior mutability via the
    /// inner lock is all a kill needs. Best-effort — the job error it
    /// accompanies is the signal that matters.
    async fn kill_wedged(&self) {
        let mut inner = self.inner.write().await;
        if let Some(ref mut process) = *inner {
            let _ = process.force_kill().await;
        }
        inner.take();
    }

    pub async fn shutdown_with_timeout(&mut self, timeout_ms: u64) -> Result<(), String> {
        let shutdown_result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            self.shutdown(),
        )
        .await;

        match shutdown_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => self.force_shutdown().await,
        }
    }

    fn spawn_stderr_reader_static(worker_id: usize, stderr: ChildStderr, route: Arc<StderrRoute>) {
        route
            .closed
            .store(false, std::sync::atomic::Ordering::Release);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                match crate::plugs::process::read_line_lossy(&mut reader, &mut buf).await {
                    Ok(None) => break,
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            route.line(worker_id, trimmed);
                        }
                    }
                    Err(e) => {
                        // `unwrap_or(0)` used to mask broken-pipe and
                        // any other IO error as EOF, silencing the
                        // exact diagnostics an operator needs when AV
                        // mid-killed the python worker.
                        log::debug!("Worker {} stderr reader stopped: {}", worker_id, e);
                        break;
                    }
                }
            }
            route
                .closed
                .store(true, std::sync::atomic::Ordering::Release);
        });
    }

    /// Kill the shell phase running on this worker, if any. Its
    /// `execute_shell_phase` then returns a stopped error.
    fn kill_running_shell(&self) {
        let mut running = self.running_shell.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = running.group_id.take() {
            running.stopped = true;
            crate::plugs::process::kill_process_group(id);
        }
    }

    pub async fn execute_job(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        event_sink: Arc<dyn EventSink>,
        attachment_dir: Option<PathBuf>,
    ) -> Result<JobResult, String> {
        log::debug!(
            "Worker {} executing {} phase: {}",
            self.id,
            match job.runtime_type {
                crate::job::RuntimeType::Native => "native Rust",
                crate::job::RuntimeType::Python => "Python",
                crate::job::RuntimeType::Shell => "shell",
            },
            job.phase_name
        );

        match job.runtime_type {
            crate::job::RuntimeType::Native => self.execute_native_phase(job, event_sink).await,
            crate::job::RuntimeType::Python => {
                self.execute_python_phase(job, plug_ports, event_sink, attachment_dir)
                    .await
            }
            crate::job::RuntimeType::Shell => self.execute_shell_phase(job).await,
        }
    }

    pub async fn execute_shell_phase(&self, job: Job) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();
        let mut logs = Vec::new();

        let command = job
            .command
            .as_ref()
            .ok_or_else(|| "`executable.command` empty".to_string())?;

        let working_dir = crate::worker::runtime::shell::resolve_working_directory(
            job.working_directory.as_deref(),
            job.procedure_dir.as_deref(),
        );

        if !working_dir.exists() {
            return Err(format!(
                "Working directory not found: {}",
                working_dir.display()
            ));
        }

        let shell_builder =
            crate::worker::runtime::shell::ShellCommandBuilder::new(job.shell_type.as_deref())?;
        let shell_type = shell_builder.shell_type().to_string();

        logs.push(crate::log::LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!(
                "Running `{}` ({}) in {}",
                command,
                shell_type,
                working_dir.display()
            ),
            file: None,
            line: None,
        });

        let mut child = shell_builder
            .command(command)
            .working_dir(&working_dir)
            .envs(&self.procedure_env)
            // The phase's own `env:` wins over the procedure's.
            .envs(&job.run.env)
            .with_stdio(
                std::process::Stdio::piped(),
                std::process::Stdio::piped(),
                std::process::Stdio::piped(),
            )
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("Shell not found in PATH: {}", shell_type)
                } else {
                    format!("Cannot run {}: {}", shell_type, e)
                }
            })?;

        let pid = child.id();
        *self.running_shell.lock().unwrap_or_else(|e| e.into_inner()) = RunningShell {
            group_id: pid,
            stopped: false,
        };

        // Cleared on every exit, the timeout's drop of this future too:
        // a stale group id would have a later stop kill a reused pid.
        let _running = RunningShellGuard(&self.running_shell);

        let waited = child.wait_with_output().await;
        if self
            .running_shell
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
        {
            return Err("Command killed".to_string());
        }
        let output = waited.map_err(|e| format!("Cannot wait for command: {}", e))?;

        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                logs.push(crate::log::LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "INFO".to_string(),
                    message: line.to_string(),
                    file: None,
                    line: None,
                });
            }
        }

        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                logs.push(crate::log::LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "ERROR".to_string(),
                    message: line.to_string(),
                    file: None,
                    line: None,
                });
            }
        }

        let shell_exit_code = output.status.code();
        let (phase_result, error) = if output.status.success() {
            logs.push(crate::log::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: "Exit 0".to_string(),
                file: None,
                line: None,
            });
            (crate::job::PhaseResult::Continue, None)
        } else {
            let exit_code = shell_exit_code.unwrap_or(-1);
            logs.push(crate::log::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "ERROR".to_string(),
                message: format!("Exit {}", exit_code),
                file: None,
                line: None,
            });
            (crate::job::PhaseResult::Fail, None)
        };

        let end_time = chrono::Utc::now();

        Ok(JobResult {
            phase_result,
            phase_outcome: crate::job::Outcome::PENDING_COMPLETION,
            next_action: None,
            timeout_secs: None,
            timeout_ms: None,
            error,
            exit_code: shell_exit_code,
            measurements: Vec::new(),
            logs,
            started_at: start_time,
            completed_at: end_time,
            resource_metrics: None,
            unit: None,
            input_unit_info: job.initial_unit_info.clone(),
            retry_count: job.retry_count,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        })
    }

    pub async fn execute_native_phase(
        &self,
        job: Job,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();

        let has_ui = !job.ui_config.components.is_empty();
        let requires_user_input = job.ui_config.requires_user_input();

        let ui_response_rx = if has_ui {
            let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());

            let ui_response_rx = if requires_user_input {
                let (tx, rx) = tokio::sync::oneshot::channel();
                crate::ui::channels::register_ui_channel(
                    request_id.clone(),
                    job.slot_id.clone(),
                    job.is_teardown(),
                    tx,
                    job.ui_config.components.clone(),
                )
                .await;

                log::debug!(
                    "Created UI response channel for native phase: {}",
                    request_id
                );
                Some((request_id.clone(), rx))
            } else {
                None
            };

            let event_data = crate::ui::UiRequestData {
                request_id: request_id.clone(),
                job_id: job.id.to_string(),
                pipe_path: String::new(),
                config: job.ui_config.clone(),
                phase_key: job.phase_key.clone(),
                slot_id: job.slot_id.clone(),
            };

            event_sink.emit(&ExecutionEvent::UiRequest(event_data));

            log::debug!(
                "Sent UI request {} for native phase {}",
                request_id,
                job.phase_name
            );

            ui_response_rx
        } else {
            None
        };

        let mut bound_measurements_to_merge: Option<HashMap<String, serde_json::Value>> = None;
        let mut unit_info: Option<crate::unit::UnitInfo> = None;

        let ui_result = if has_ui && requires_user_input {
            if let Some((request_id, rx)) = ui_response_rx {
                let outcome = match rx.await {
                    Ok(ui_values) => {
                        if let Some((ui_unit, bound)) =
                            extract_bound_measurements(&ui_values, &job.ui_config.components)
                        {
                            // Mid-run native-phase identify: merge with
                            // the unit known when this job started so a
                            // partial scan (e.g. just a sub-unit serial)
                            // doesn't clobber pre-run-set fields. The
                            // python-phase branch above does the same;
                            // both paths must publish the merged result
                            // so wire consumers' `mergeUnit` reducer
                            // produces the same final state regardless
                            // of which framework the operator used.
                            if let Some(u) = ui_unit {
                                let merged = merge_unit_info(job.initial_unit_info.clone(), u);
                                event_sink.emit(&ExecutionEvent::UnitIdentified {
                                    slot_id: job.slot_id.clone(),
                                    unit_info: merged.clone(),
                                });
                                unit_info = Some(merged);
                            }
                            bound_measurements_to_merge = Some(bound);
                        }

                        (crate::job::PhaseResult::Continue, None)
                    }
                    // Closed because nobody can answer (headless run): the
                    // phase failed, it was not stopped. Every other close
                    // is a stop and keeps STOP (`on_first_failure: stop`).
                    Err(_) => {
                        match crate::ui::channels::take_unanswerable_reason(&request_id).await {
                            Some(reason) => (crate::job::PhaseResult::Continue, Some(reason)),
                            None => (crate::job::PhaseResult::Stop, None),
                        }
                    }
                };
                // Answered or closed under us: either way the registry
                // entry is done with. The CLI removes only the sender.
                crate::ui::channels::unregister_ui_channel(&request_id).await;
                outcome
            } else {
                (
                    crate::job::PhaseResult::Continue,
                    Some("No UI response channel available".to_string()),
                )
            }
        } else {
            (crate::job::PhaseResult::Continue, None)
        };

        let (phase_result, execution_error) = ui_result;
        // The prompt closed under a stop someone else made: the phase reads
        // STOP without having asked for it.
        let prompt_closed = matches!(phase_result, crate::job::PhaseResult::Stop);

        let end_time = chrono::Utc::now();

        let mut job_result = JobResult {
            phase_result,
            phase_outcome: crate::job::Outcome::PENDING_COMPLETION,
            next_action: None,
            timeout_secs: None,
            timeout_ms: None,
            error: execution_error,
            exit_code: None,
            measurements: Vec::new(),
            logs: Vec::new(),
            started_at: start_time,
            completed_at: end_time,
            resource_metrics: None,
            unit: unit_info,
            input_unit_info: job.initial_unit_info.clone(),
            retry_count: job.retry_count,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed,
        };
        finalize_and_announce(
            &mut job_result,
            bound_measurements_to_merge,
            &job,
            event_sink.as_ref(),
        );
        Ok(job_result)
    }
}

/// Show the phase's YAML screen: its press sink always, and its answer
/// channel when the screen `waits` for the operator (display-only
/// screens are shown, not waited on).
async fn open_screen(
    job: &Job,
    registered: &mut RegisteredPrompts,
    press_names: &[String],
    press_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    event_sink: &dyn EventSink,
    waits: bool,
) -> Option<(
    String,
    tokio::sync::oneshot::Receiver<HashMap<String, String>>,
)> {
    let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());
    registered.0.push(request_id.clone());
    let answer = if waits {
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::ui::channels::register_ui_channel(
            request_id.clone(),
            job.slot_id.clone(),
            job.is_teardown(),
            tx,
            job.ui_config.components.clone(),
        )
        .await;
        Some(rx)
    } else {
        None
    };
    crate::ui::channels::register_press_sink(&request_id, press_names.to_vec(), press_tx.clone());
    event_sink.emit(&ExecutionEvent::UiRequest(crate::ui::UiRequestData {
        request_id: request_id.clone(),
        job_id: job.id.to_string(),
        pipe_path: String::new(),
        config: job.ui_config.clone(),
        phase_key: job.phase_key.clone(),
        slot_id: job.slot_id.clone(),
    }));
    log::debug!(
        "Sent UI request {} for Python phase {}",
        request_id,
        job.phase_name
    );
    answer.map(|rx| (request_id, rx))
}

/// The command the Python worker runs the phase from.
fn job_command(
    job: &Job,
    plug_ports: HashMap<String, u16>,
    press_names: Vec<String>,
) -> protocol::JobCommand {
    // Build unit_info for NDJSON if available
    let ndjson_unit_info = job.initial_unit_info.as_ref().map(|ui| protocol::UnitInfo {
        serial_number: ui.serial_number.clone(),
        part_number: ui.part_number.clone(),
        revision_number: ui.revision_number.clone(),
        batch_number: ui.batch_number.clone(),
        operated_by: ui.operated_by.clone(),
        sub_units: ui.sub_units.clone().unwrap_or_default(),
        metadata: ui.metadata.clone(),
    });

    // Build command. The Python worker doesn't read UI fields off
    // the command (mid-phase mutations come back via UiUpdate
    // events keyed by attribute name on `ui.<key>`), so the
    // operator-UI config is omitted from the IPC payload entirely.
    protocol::JobCommand {
        job_id: job.id.to_string(),
        slot_id: job
            .slot_id
            .clone()
            .unwrap_or_else(|| "<shared>".to_string()),
        phase_name: job.phase_name.clone(),
        module: job.module.clone(),
        function: job.function.clone(),
        plugs: plug_ports
            .into_iter()
            .map(|(k, v)| {
                (
                    crate::python::to_python_identifier(&k),
                    format!("127.0.0.1:{}", v),
                )
            })
            .collect(),
        timeout_ms: job.timeout_ms,
        retry_count: job.retry_count as u32,
        retry_limit: job.retry_limit as u32,
        unit_info: ndjson_unit_info,
        phase_results: job.phase_results.clone(),
        ui_components: ui_component_specs(&job.ui_config.components),
        ui_panel: job.ui_config.panel.is_some(),
        ui_events: press_names,
        inputs: job
            .inputs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        // Alias targets are plug KEYS; the `plugs` map above is keyed by
        // the Python identifier of the key, so spell them the same way.
        plug_aliases: job
            .plug_aliases
            .iter()
            .map(|(param, key)| (param.clone(), crate::python::to_python_identifier(key)))
            .collect(),
        with: job
            .run
            .with
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        env: job.run.env.iter().cloned().collect(),
        capture_locals: job.capture_locals,
        unavailable_plugs: job
            .unavailable_plugs
            .iter()
            .map(|(k, why)| (crate::python::to_python_identifier(k), why.clone()))
            .collect(),
        run_context: job.run.context.clone(),
        config: job
            .run
            .config
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        secrets: job
            .run
            .secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

/// `finalize_measurements`, then each answer a `bind:` recorded told
/// live like a measurement the phase code wrote: a screen or an agent
/// following `measurement_recorded` sees every value, not only the
/// phase's own.
fn finalize_and_announce(
    job_result: &mut JobResult,
    ui_bound: Option<HashMap<String, serde_json::Value>>,
    job: &Job,
    event_sink: &dyn EventSink,
) {
    let before = job_result.measurements.len();
    finalize_measurements(job_result, ui_bound, &job.phase_measurements);
    for m in job_result.measurements[before..]
        .iter()
        .filter(|m| !matches!(m.value, crate::measurements::MeasurementValue::Null))
    {
        event_sink.emit(&ExecutionEvent::MeasurementRecorded {
            job_id: job.id.to_string(),
            phase_key: job.phase_key.clone(),
            slot_id: job.slot_id.clone(),
            name: m.name.clone(),
            value: serde_json::to_value(&m.value).unwrap_or(serde_json::Value::Null),
            unit: m.unit.clone(),
            timestamp: m.timestamp.clone(),
            outcome: m.outcome.clone(),
            validators: m.validators.clone().unwrap_or_default(),
        });
    }
}

/// Merge the operator's values into the phase's measurements, then add the
/// declared ones nothing recorded. The order is the contract: a placeholder
/// added first would hold the name, and the operator's value would be
/// dropped as a duplicate. On a name both sources recorded, Python wins.
fn finalize_measurements(
    job_result: &mut JobResult,
    ui_bound: Option<HashMap<String, serde_json::Value>>,
    declared: &[crate::procedure::schema::MeasurementSpec],
) {
    if let Some(bound) = ui_bound {
        let (evaluated, issues) = crate::measurements::auto_evaluate_measurements(
            convert_bound_to_measurements(bound),
            &declared_phase(declared),
        );
        job_result
            .logs
            .extend(issues.iter().map(|i| i.to_log_entry()));
        let recorded: std::collections::HashSet<String> = job_result
            .measurements
            .iter()
            .map(|m| m.name.clone())
            .collect();
        job_result.measurements.extend(
            evaluated
                .into_iter()
                .filter(|m| !recorded.contains(&m.name)),
        );
    }
    complete_declared(job_result, declared);
}

/// A phase carrying only its measurement declarations, the one part
/// `auto_evaluate_measurements` reads.
fn declared_phase(
    declared: &[crate::procedure::schema::MeasurementSpec],
) -> crate::procedure::schema::PhaseDefinition {
    crate::procedure::schema::PhaseDefinition {
        measurements: declared.to_vec(),
        key: String::new(),
        sequence_path: Vec::new(),
        sequence_stages: Vec::new(),
        inputs: Default::default(),
        plug_aliases: Default::default(),
        source: None,
        written: None,
        name: String::new(),
        scope: None,
        python: None,
        executable: None,
        description: None,
        depends_on: Vec::new(),
        ui: None,
        enabled: true,
        result: None,
        timeout: None,
        retry: None,
        then: None,
        with: Default::default(),
        env: Default::default(),
        value_scope: None,
    }
}

/// Add the declared measurements the phase never recorded, and report
/// them. Only for a phase that ran to its end: a skipped phase, one that
/// returned early to retry or stop, or one that crashed had a reason not
/// to record, and the report would only restate that.
fn complete_declared(
    job_result: &mut JobResult,
    declared: &[crate::procedure::schema::MeasurementSpec],
) {
    if job_result.error.is_some()
        || !matches!(job_result.phase_result, crate::job::PhaseResult::Continue)
    {
        return;
    }
    let issues =
        crate::measurements::complete_declared_measurements(&mut job_result.measurements, declared);
    job_result
        .logs
        .extend(issues.iter().map(|i| i.to_log_entry()));
}

/// Convert protocol JobResult to internal JobResult
fn convert_job_result(
    result: protocol::JobResult,
    start_time: chrono::DateTime<chrono::Utc>,
    end_time: chrono::DateTime<chrono::Utc>,
    job: &Job,
) -> Result<crate::job::JobResult, String> {
    use crate::job::PhaseResult;

    let phase_result = result
        .phase_result_json
        .as_ref()
        .and_then(|json| serde_json::from_str(json).ok())
        .and_then(|pr| PhaseResult::from_python_result(&pr).ok())
        .unwrap_or(PhaseResult::Continue);

    let measurements: Vec<crate::measurements::Measurement> = result
        .measurements
        .into_iter()
        .filter_map(try_measurement_from_protocol)
        .collect();

    let (evaluated_measurements, issues) = crate::measurements::auto_evaluate_measurements(
        measurements,
        &declared_phase(&job.phase_measurements),
    );

    let logs = result
        .logs
        .into_iter()
        .map(|l| crate::log::LogEntry {
            timestamp: l.timestamp,
            level: l.level,
            message: l.message,
            file: l.file,
            line: l.line,
        })
        .chain(issues.iter().map(|i| i.to_log_entry()))
        .collect();

    let unit = result
        .unit_json
        .and_then(|json| match serde_json::from_str(&json) {
            Ok(u) => Some(u),
            Err(e) => {
                log::debug!("Invalid unit_json: {} (json: {})", e, json);
                None
            }
        });

    Ok(crate::job::JobResult {
        phase_result,
        phase_outcome: crate::job::Outcome::PENDING_COMPLETION,
        next_action: None,
        timeout_secs: None,
        timeout_ms: None,
        error: result.error,
        exit_code: result.exit_code,
        measurements: evaluated_measurements,
        logs,
        started_at: start_time,
        completed_at: end_time,
        resource_metrics: Default::default(),
        unit,
        input_unit_info: job.initial_unit_info.clone(),
        retry_count: job.retry_count,
        run_metadata: parse_metadata_json("run_metadata_json", result.run_metadata_json),
        unit_metadata: parse_metadata_json("unit_metadata_json", result.unit_metadata_json),
        failure_locals: result
            .locals_json
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default(),
        prompt_closed: false,
    })
}

/// Parse a double-encoded metadata JSON object from the worker. Metadata
/// must never crash a run — malformed JSON is logged and dropped, same
/// failure mode as `unit_json`.
fn parse_metadata_json(
    label: &str,
    json: Option<String>,
) -> std::collections::HashMap<String, serde_json::Value> {
    json.and_then(|j| match serde_json::from_str(&j) {
        Ok(m) => Some(m),
        Err(e) => {
            log::debug!("Invalid {}: {} (json: {})", label, e, j);
            None
        }
    })
    .unwrap_or_default()
}

fn extract_unit_info_from_json(
    unit_data: &serde_json::Map<String, serde_json::Value>,
) -> crate::unit::UnitInfo {
    let serial_number = unit_data
        .get("serial_number")
        .and_then(|v| v.as_str())
        .map(String::from);
    let batch_number = unit_data
        .get("batch_number")
        .and_then(|v| v.as_str())
        .map(String::from);
    let part_number = unit_data
        .get("part_number")
        .and_then(|v| v.as_str())
        .map(String::from);
    let revision_number = unit_data
        .get("revision_number")
        .and_then(|v| v.as_str())
        .map(String::from);

    let sub_units = unit_data.get("sub_units").and_then(|v| {
        if let Some(obj) = v.as_object() {
            let parsed: std::collections::HashMap<String, String> = obj
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        } else {
            None
        }
    });

    crate::unit::UnitInfo {
        serial_number,
        batch_number,
        part_number,
        revision_number,
        // Run attribution never rides `__unit__` — it arrives via the
        // separate `__run__` bound payload (see extract_bound_measurements).
        operated_by: None,
        sub_units,
        status: "tested".to_string(),
        // Python-set unit metadata flows via the JobComplete bundle
        // (unit_metadata_json), not the __unit__ bound payload.
        metadata: None,
    }
}

/// What the Python worker needs to refuse a bad `ui.set` call with a
/// readable exception: each component's key and type, and the properties
/// a phase may set on it. Derived here, from the one allow-list in
/// `station_protocol::properties`, so Python never carries its own copy.
fn ui_component_specs(components: &[crate::ui::UiComponent]) -> Vec<protocol::UiComponentSpec> {
    components
        .iter()
        .map(|c| protocol::UiComponentSpec {
            key: c.key.clone(),
            component_type: c.component_type.as_str().to_string(),
            settable: station_protocol::properties::settable_properties(c.component_type)
                .into_iter()
                .map(str::to_string)
                .collect(),
        })
        .collect()
}

/// Patch the component a `ui.set` payload names, whole or not at all
/// (`UiPropertyPatch::apply` refuses whole), and return its new state.
fn patch_component(
    screen: &mut [crate::ui::UiComponent],
    payload: &station_protocol::UiSetPropertiesPayload,
) -> Result<crate::ui::UiComponent, String> {
    let Some(component) = screen.iter_mut().find(|c| c.key == payload.id) else {
        let keys: Vec<&str> = screen.iter().map(|c| c.key.as_str()).collect();
        return Err(format!(
            "ui.set('{}'): no component {} (have: {})",
            payload.id,
            payload.id,
            keys.join(", ")
        ));
    };
    payload.properties.apply(component)?;
    Ok(component.clone())
}

/// A `ui.set` payload, parsed once for both screens it patches: the one
/// on display and the one a later `ui.wait()` re-arms.
fn parse_set_properties(
    data_json: &str,
) -> Result<station_protocol::UiSetPropertiesPayload, String> {
    serde_json::from_str(data_json).map_err(|e| format!("ui.set: invalid payload: {e}"))
}

/// Apply a `set_properties` update from the phase to the screen the
/// operator is answering.
///
/// With a pending prompt (`request_id`), the patch lands on
/// `PendingUi.components` under the registry lock `send_validated` takes,
/// and `live` is mirrored in the same critical section, so no submit can
/// be checked against a half-updated screen. `Ok(None)` means the prompt
/// is no longer pending: the operator already answered, and a late
/// update is a no-op by design. Without a prompt (display-only screen)
/// only `live` is patched.
///
/// `Err` is a refusal: the payload does not parse, the key is not on the
/// screen, or `UiPropertyPatch::apply` rejected it. Nothing changed on
/// either side. Python already refuses the shape and allow-list errors
/// before sending, so a refusal here is a cross-field rule (`min > max`,
/// a pattern the station cannot compile) or a protocol bug.
///
/// `Ok(Some(payload))` is the normalized payload to emit to the surfaces.
#[cfg(test)]
async fn apply_ui_set_properties(
    request_id: Option<&str>,
    data_json: &str,
    live: &mut [crate::ui::UiComponent],
) -> Result<Option<station_protocol::UiSetPropertiesPayload>, String> {
    apply_parsed_set_properties(request_id, parse_set_properties(data_json)?, live).await
}

/// The body of `apply_ui_set_properties` (documented above), on a payload
/// the job loop already parsed.
async fn apply_parsed_set_properties(
    request_id: Option<&str>,
    payload: station_protocol::UiSetPropertiesPayload,
    live: &mut [crate::ui::UiComponent],
) -> Result<Option<station_protocol::UiSetPropertiesPayload>, String> {
    let patch_in =
        |components: &mut [crate::ui::UiComponent]| patch_component(components, &payload);

    let patched = match request_id {
        Some(request_id) => {
            let mut channels = crate::ui::UI_RESPONSE_CHANNELS.lock().await;
            let Some(pending) = channels.get_mut(request_id) else {
                return Ok(None);
            };
            let patched = patch_in(&mut pending.components)?;
            // Mirror while still holding the lock.
            if let Some(mine) = live.iter_mut().find(|c| c.key == payload.id) {
                *mine = patched.clone();
            }
            patched
        }
        None => patch_in(live)?,
    };
    debug_assert_eq!(patched.key, payload.id);
    Ok(Some(payload))
}

fn extract_bound_measurements(
    ui_values: &HashMap<String, String>,
    components: &[crate::ui::UiComponent],
) -> Option<(
    Option<crate::unit::UnitInfo>,
    HashMap<String, serde_json::Value>,
)> {
    let bound_json = ui_values.get("__bound_measurements__")?;
    let mut bound: HashMap<String, serde_json::Value> = serde_json::from_str(bound_json).ok()?;
    // Clients submit the operator's typed input; the recorded value for
    // affixed text inputs is `prefix + input + suffix` — composed here,
    // once, so every surface (web, TUI, agent, Studio) records the same
    // value. Same contract as identify_unit/resolve.rs.
    crate::ui::compose_bound_affixes(components, &mut bound);

    let mut unit_info = if let Some(unit_value) = bound.remove("__unit__") {
        let unit_data_opt = match &unit_value {
            serde_json::Value::Object(obj) => Some(obj.clone()),
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        };

        unit_data_opt.map(|unit_data| extract_unit_info_from_json(&unit_data))
    } else {
        None
    };

    // `run.operated_by` bind: run attribution arrives under `__run__`.
    // It shares the UnitInfo carriage (see unit.rs) so the same merge
    // and UnitIdentified re-publish carry it to the upload.
    if let Some(run_value) = bound.remove("__run__") {
        let run_data: Option<serde_json::Map<String, serde_json::Value>> = match &run_value {
            serde_json::Value::Object(obj) => Some(obj.clone()),
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        };
        let operated_by = run_data
            .as_ref()
            .and_then(|d| d.get("operated_by"))
            .and_then(|v| v.as_str())
            // A cleared bound text input ships "" — blank is "operator
            // left it empty", not an attribution to record.
            .filter(|s| !s.trim().is_empty())
            .map(String::from);
        if operated_by.is_some() {
            unit_info
                .get_or_insert_with(|| crate::unit::UnitInfo {
                    serial_number: None,
                    part_number: None,
                    revision_number: None,
                    batch_number: None,
                    operated_by: None,
                    sub_units: None,
                    status: "tested".to_string(),
                    metadata: None,
                })
                .operated_by = operated_by;
        }
    }

    Some((unit_info, bound))
}

fn convert_bound_to_measurements(
    bound: HashMap<String, serde_json::Value>,
) -> Vec<crate::measurements::Measurement> {
    bound
        .into_iter()
        .map(|(name, value)| {
            let measurement_value = match value {
                serde_json::Value::Null => crate::measurements::MeasurementValue::Null,
                serde_json::Value::Bool(b) => crate::measurements::MeasurementValue::Boolean(b),
                serde_json::Value::Number(n) => {
                    crate::measurements::MeasurementValue::Numeric(n.as_f64().unwrap_or(0.0))
                }
                serde_json::Value::String(s) => crate::measurements::MeasurementValue::String(s),
                serde_json::Value::Array(arr) => crate::measurements::MeasurementValue::Array(arr),
                serde_json::Value::Object(obj) => {
                    crate::measurements::MeasurementValue::Object(obj)
                }
            };

            crate::measurements::Measurement {
                name: name.clone(),
                value: measurement_value,
                unit: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                validators: None,
                aggregations: None,
                description: None,
                outcome: crate::procedure::schema::ValidatorOutcome::Unset,
            }
        })
        .collect()
}

/// True when a Python phase's returned unit identity differs from what
/// the phase received — the signal that user code mutated `unit.<field>`
/// and wire consumers (operator-UI, upload payload) must learn the new
/// values. `status` and `metadata` are excluded: status flips
/// "complete"→"tested" on every phase, and Python metadata rides the
/// JobComplete bundle instead.
fn unit_identity_changed(
    result: &crate::unit::UnitInfo,
    input: Option<&crate::unit::UnitInfo>,
) -> bool {
    let scalar = |f: fn(&crate::unit::UnitInfo) -> &Option<String>| -> bool {
        f(result) != input.map(f).unwrap_or(&None)
    };
    scalar(|u| &u.serial_number)
        || scalar(|u| &u.part_number)
        || scalar(|u| &u.revision_number)
        || scalar(|u| &u.batch_number)
        || scalar(|u| &u.operated_by)
        || result.sub_units.clone().unwrap_or_default()
            != input.and_then(|u| u.sub_units.clone()).unwrap_or_default()
}

fn merge_unit_info(
    existing: Option<crate::unit::UnitInfo>,
    ui_unit: crate::unit::UnitInfo,
) -> crate::unit::UnitInfo {
    match existing {
        Some(mut base) => {
            if let Some(ui_sub_units) = ui_unit.sub_units {
                let mut merged_sub_units = base.sub_units.unwrap_or_default();
                for (key, value) in ui_sub_units {
                    merged_sub_units.insert(key, value);
                }
                base.sub_units = Some(merged_sub_units);
            }
            if ui_unit.serial_number.is_some() {
                base.serial_number = ui_unit.serial_number;
            }
            if ui_unit.part_number.is_some() {
                base.part_number = ui_unit.part_number;
            }
            if ui_unit.revision_number.is_some() {
                base.revision_number = ui_unit.revision_number;
            }
            if ui_unit.batch_number.is_some() {
                base.batch_number = ui_unit.batch_number;
            }
            if ui_unit.operated_by.is_some() {
                base.operated_by = ui_unit.operated_by;
            }
            if let Some(ui_md) = ui_unit.metadata {
                base.metadata
                    .get_or_insert_with(Default::default)
                    .extend(ui_md);
            }
            base
        }
        None => ui_unit,
    }
}

#[cfg(test)]
mod debug_env_tests {
    use super::build_worker_env;

    #[test]
    fn no_debug_port_sets_only_worker_id() {
        let env = build_worker_env(3, None, &[]);
        assert_eq!(env, vec![("WORKER_ID".to_string(), "3".to_string())]);
        assert!(!env.iter().any(|(k, _)| k == "TP_DEBUG"));
    }

    #[test]
    fn debug_port_sets_tp_debug_and_port() {
        let env = build_worker_env(0, Some(5678), &[]);
        assert!(env.contains(&("TP_DEBUG".to_string(), "1".to_string())));
        assert!(env.contains(&("TP_DEBUG_PORT".to_string(), "5678".to_string())));
    }

    #[test]
    fn debug_port_honors_custom_port() {
        let env = build_worker_env(0, Some(9999), &[]);
        assert!(env.contains(&("TP_DEBUG_PORT".to_string(), "9999".to_string())));
    }

    #[test]
    fn procedure_env_is_appended_after_the_worker_vars() {
        let creds = vec![("TOFUPILOT_API_KEY".to_string(), "k".to_string())];
        let env = build_worker_env(1, None, &creds);
        assert_eq!(
            env,
            vec![
                ("WORKER_ID".to_string(), "1".to_string()),
                ("TOFUPILOT_API_KEY".to_string(), "k".to_string()),
            ]
        );
    }

    #[test]
    fn with_procedure_env_builder_threads_into_worker() {
        let w = super::Worker::new(0, std::path::PathBuf::from("/tmp"));
        assert!(w.procedure_env.is_empty());
        let creds = vec![("TOFUPILOT_URL".to_string(), "https://x".to_string())];
        let w = w.with_procedure_env(creds.clone());
        assert_eq!(w.procedure_env, creds);
    }

    #[test]
    fn with_debug_port_builder_threads_into_env() {
        // Pool workers opt in; the default (teardown/respawn) stays clean.
        let plain = super::Worker::new(0, std::path::PathBuf::from("/tmp"));
        assert_eq!(plain.debug_port, None);
        let dbg = plain.with_debug_port(Some(5678));
        assert_eq!(dbg.debug_port, Some(5678));
    }
}

#[cfg(test)]
mod stage_read_tests {
    use super::{read_stage_event, JobStage, StageRead};
    use tokio::io::AsyncWriteExt;

    /// Duplex-backed buffered reader plus the writer that feeds it.
    fn pipe() -> (
        tokio::io::BufReader<tokio::io::DuplexStream>,
        tokio::io::DuplexStream,
    ) {
        let (rx, tx) = tokio::io::duplex(64 * 1024);
        (tokio::io::BufReader::new(rx), tx)
    }

    #[tokio::test(start_paused = true)]
    async fn silent_worker_stalls_at_the_ack_deadline() {
        // The field symptom: command sent, interpreter alive but never
        // executing. tokio's paused clock auto-advances past the 30s
        // deadline the moment the read has nothing else to do.
        let (mut reader, _tx) = pipe();
        let out = read_stage_event(&mut reader, JobStage::AwaitingAck, false).await;
        assert!(matches!(out, StageRead::Stalled));
    }

    #[tokio::test(start_paused = true)]
    async fn ack_before_deadline_is_delivered_not_killed() {
        // Anti-false-kill: a slow-but-alive worker whose ack lands
        // inside the window must never be treated as a stall.
        let (mut reader, mut tx) = pipe();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(25)).await;
            let _ = tx
                .write_all(b"{\"type\":\"JobAck\",\"job_id\":\"j1\"}\n")
                .await;
        });
        let out = read_stage_event(&mut reader, JobStage::AwaitingAck, false).await;
        match out {
            StageRead::Event(event)
                if matches!(*event, super::protocol::WorkerEvent::JobAck(_)) =>
            {
                let super::protocol::WorkerEvent::JobAck(a) = *event else {
                    unreachable!()
                };
                assert_eq!(a.job_id, "j1");
            }
            other => panic!("expected JobAck, got {:?}", stage_read_kind(&other)),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn executing_stage_never_deadlines() {
        // Once user code owns the clock the read must park indefinitely:
        // race an hour of virtual time against the unbounded read and
        // require the timer to win. If Executing ever grew a deadline,
        // the read would resolve first and this test would catch it.
        let (mut reader, _tx) = pipe();
        let long = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::select! {
            _ = long => {}
            _ = read_stage_event(&mut reader, JobStage::Executing, false) => {
                panic!("Executing-stage read resolved without input");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn debug_mode_disables_the_ack_deadline() {
        // A debugger paused before the first event must not be killed.
        let (mut reader, _tx) = pipe();
        let long = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::select! {
            _ = long => {}
            _ = read_stage_event(&mut reader, JobStage::AwaitingAck, true) => {
                panic!("debug-mode read resolved without input");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn eof_is_eof_not_a_stall() {
        // A crashed worker closes the socket — that's the existing
        // crash path ("stream ended without job completion"), and must
        // stay distinguishable from alive-but-silent.
        let (mut reader, tx) = pipe();
        drop(tx);
        let out = read_stage_event(&mut reader, JobStage::AwaitingAck, false).await;
        assert!(matches!(out, StageRead::Eof));
    }

    #[tokio::test(start_paused = true)]
    async fn the_reader_task_moves_the_stage_on_events() {
        // The stall deadline follows the stage the read was made in: an
        // ack moves AwaitingAck to Loading, so silence after it is an
        // import stall; ModuleLoaded lifts every deadline.
        let (reader, mut tx) = pipe();
        let (mut events, _task) = super::spawn_event_reader(reader, false);
        tx.write_all(b"{\"type\":\"JobAck\",\"job_id\":\"j1\"}\n")
            .await
            .unwrap();
        let (first, stage) = events.recv().await.unwrap();
        assert!(matches!(first, StageRead::Event(_)));
        assert_eq!(stage, JobStage::AwaitingAck);
        let (second, stage) = events.recv().await.unwrap();
        assert!(matches!(second, StageRead::Stalled));
        assert_eq!(stage, JobStage::Loading);
        assert!(events.recv().await.is_none(), "the task ends after a stall");

        let (reader, mut tx) = pipe();
        let (mut events, _task) = super::spawn_event_reader(reader, false);
        tx.write_all(b"{\"type\":\"ModuleLoaded\",\"job_id\":\"j1\"}\n")
            .await
            .unwrap();
        let (_, stage) = events.recv().await.unwrap();
        assert_eq!(stage, JobStage::AwaitingAck);
        let long = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::select! {
            _ = long => {}
            _ = events.recv() => panic!("an Executing read resolved without input"),
        }
    }

    fn stage_read_kind(r: &StageRead) -> &'static str {
        match r {
            StageRead::Event(_) => "Event",
            StageRead::Eof => "Eof",
            StageRead::Io(_) => "Io",
            StageRead::Stalled => "Stalled",
        }
    }
}

#[cfg(test)]
mod job_stage_tests {
    use super::{JobStage, STALL_MARKER_ACK, STALL_MARKER_IMPORT};
    use crate::constants::timeouts::{JOB_ACK_TIMEOUT_SECS, MODULE_IMPORT_TIMEOUT_SECS};

    fn test_job() -> crate::job::Job {
        crate::job::Job::new(
            crate::job::JobSpec::bare(
                Some("default".to_string()),
                "stall_phase",
                "Stall Phase",
                crate::procedure::schema::StageScope::Main,
            ),
            "phases.stall".to_string(),
            "stall".to_string(),
            &std::collections::HashMap::new(),
        )
    }

    #[test]
    fn awaiting_ack_is_bounded_by_ack_timeout() {
        assert_eq!(
            JobStage::AwaitingAck.read_deadline(),
            Some(std::time::Duration::from_secs(JOB_ACK_TIMEOUT_SECS))
        );
    }

    #[test]
    fn loading_is_bounded_by_module_import_timeout() {
        assert_eq!(
            JobStage::Loading.read_deadline(),
            Some(std::time::Duration::from_secs(MODULE_IMPORT_TIMEOUT_SECS))
        );
    }

    #[test]
    fn executing_is_unbounded() {
        // Once user code owns the clock, the Rust side must never
        // deadline the read — burn-in phases legitimately run for hours.
        assert_eq!(JobStage::Executing.read_deadline(), None);
    }

    #[test]
    fn stall_errors_name_phase_and_trigger_pool_replacement() {
        let job = test_job();
        let ack = JobStage::AwaitingAck.stall_error(&job);
        let load = JobStage::Loading.stall_error(&job);
        assert!(ack.contains("Stall Phase"));
        assert!(load.contains("Stall Phase"));
        assert!(load.contains("phases.stall"));
        // The orchestrator's worker-replacement branch keys on these
        // substrings (`execution.rs`); drifting them detaches the wedged
        // worker's replacement and every later phase lands on a corpse.
        assert!(ack.contains(STALL_MARKER_ACK));
        assert!(load.contains(STALL_MARKER_IMPORT));
    }
}

#[cfg(test)]
mod metadata_json_tests {
    use super::parse_metadata_json;

    #[test]
    fn valid_double_encoded_object_parses() {
        let json = r#"{"modification":"MOD-42","cycles":3,"calibrated":true}"#;
        let map = parse_metadata_json("run_metadata_json", Some(json.to_string()));
        assert_eq!(map.get("modification"), Some(&serde_json::json!("MOD-42")));
        assert_eq!(map.get("cycles"), Some(&serde_json::json!(3)));
        assert_eq!(map.get("calibrated"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn missing_field_is_empty() {
        let map = parse_metadata_json("run_metadata_json", None);
        assert!(map.is_empty());
    }

    #[test]
    fn malformed_json_is_empty_not_error() {
        let map = parse_metadata_json("unit_metadata_json", Some("{not json".to_string()));
        assert!(map.is_empty());
    }
}

#[cfg(test)]
mod attachment_mimetype_tests {
    use super::guess_attachment_mimetype;

    #[test]
    fn images_get_an_image_mimetype() {
        // The operator-UI keys `<img>` rendering on `image/*`.
        assert_eq!(
            guess_attachment_mimetype("board.png").as_deref(),
            Some("image/png")
        );
        assert_eq!(
            guess_attachment_mimetype("shot.JPG").as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            guess_attachment_mimetype("scan.webp").as_deref(),
            Some("image/webp")
        );
    }

    #[test]
    fn non_images_and_unknowns_are_not_image_typed() {
        assert_eq!(
            guess_attachment_mimetype("log.csv").as_deref(),
            Some("text/csv")
        );
        // Unknown extension / no extension → octet-stream, never None: the
        // upload PUT must carry a real Content-Type so the server's HeadObject
        // records a usable type. Non-image essence still keeps the UI from
        // rendering a broken <img>.
        assert_eq!(
            guess_attachment_mimetype("data.bin").as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(
            guess_attachment_mimetype("noext").as_deref(),
            Some("application/octet-stream")
        );
    }
}

#[cfg(test)]
mod finalize_measurements_tests {
    use super::finalize_measurements;
    use crate::job::{JobResult, PhaseResult};
    use crate::measurements::{Measurement, MeasurementValue};
    use crate::procedure::schema::{MeasurementSpec, PhaseDefinition, ValidatorOutcome};
    use std::collections::HashMap;

    fn declared_voltage() -> Vec<MeasurementSpec> {
        let phase: PhaseDefinition = crate::procedure::yaml::from_str(
            r#"
key: power_check
name: Power check
measurements:
  - key: voltage
    name: Voltage
    validators: [{ operator: ">", expected_value: 3.2 }]
"#,
        )
        .expect("phase yaml");
        phase.measurements
    }

    fn finished(phase_result: PhaseResult) -> JobResult {
        let mut result = JobResult::new_error(String::new());
        result.error = None;
        result.phase_result = phase_result;
        result
    }

    fn operator(voltage: f64) -> Option<HashMap<String, serde_json::Value>> {
        Some(HashMap::from([("voltage".to_string(), voltage.into())]))
    }

    fn voltage(result: &JobResult) -> &Measurement {
        let named: Vec<_> = result
            .measurements
            .iter()
            .filter(|m| m.name == "voltage")
            .collect();
        assert_eq!(named.len(), 1, "one voltage measurement: {named:?}");
        named[0]
    }

    #[test]
    fn operator_value_for_a_declared_measurement_is_kept() {
        let mut result = finished(PhaseResult::Continue);
        finalize_measurements(&mut result, operator(3.3), &declared_voltage());

        let m = voltage(&result);
        assert!(matches!(m.value, MeasurementValue::Numeric(v) if v == 3.3));
        assert_eq!(m.outcome, ValidatorOutcome::Pass);
        assert!(result.logs.is_empty(), "{:?}", result.logs);
    }

    #[test]
    fn python_value_wins_over_the_operator_value() {
        let mut result = finished(PhaseResult::Continue);
        result.measurements.push(Measurement {
            name: "voltage".to_string(),
            value: MeasurementValue::Numeric(3.4),
            unit: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
            validators: None,
            aggregations: None,
            description: None,
            outcome: ValidatorOutcome::Unset,
        });
        finalize_measurements(&mut result, operator(3.0), &declared_voltage());

        assert!(matches!(voltage(&result).value, MeasurementValue::Numeric(v) if v == 3.4));
    }

    #[test]
    fn declared_measurement_nobody_recorded_is_added_empty_and_failed() {
        let mut result = finished(PhaseResult::Continue);
        finalize_measurements(&mut result, None, &declared_voltage());

        let m = voltage(&result);
        assert!(matches!(m.value, MeasurementValue::Null));
        assert_eq!(m.outcome, ValidatorOutcome::Fail);
        assert_eq!(result.logs.len(), 1);
    }

    #[test]
    fn phase_that_returned_early_gets_no_placeholder() {
        for early in [
            PhaseResult::Retry,
            PhaseResult::Skip,
            PhaseResult::Stop,
            PhaseResult::Fail,
        ] {
            let mut result = finished(early.clone());
            finalize_measurements(&mut result, None, &declared_voltage());
            assert!(result.measurements.is_empty(), "{early:?}");
            assert!(result.logs.is_empty(), "{early:?}");
        }
    }

    #[test]
    fn phase_that_errored_gets_no_placeholder() {
        let mut result = finished(PhaseResult::Continue);
        result.error = Some("Traceback: rail did not come up".to_string());
        finalize_measurements(&mut result, None, &declared_voltage());

        assert!(result.measurements.is_empty());
        assert!(result.logs.is_empty());
    }
}

#[cfg(test)]
mod ui_set_properties_tests {
    use super::{apply_ui_set_properties, ui_component_specs};
    use crate::ui::channels::register_ui_channel;
    use crate::ui::{ComponentType, UiComponent, UiOption, UI_RESPONSE_CHANNELS};
    use std::collections::HashMap;

    fn select(key: &str) -> UiComponent {
        UiComponent {
            key: key.into(),
            required: true,
            ..UiComponent::new(ComponentType::Select)
        }
    }

    fn text_input(key: &str) -> UiComponent {
        UiComponent {
            key: key.into(),
            prefix: Some("A-".into()),
            ..UiComponent::new(ComponentType::TextInput)
        }
    }

    async fn pending(
        request_id: &str,
        slot: Option<&str>,
        components: Vec<UiComponent>,
    ) -> tokio::sync::oneshot::Receiver<HashMap<String, String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        register_ui_channel(
            request_id.into(),
            slot.map(str::to_string),
            false,
            tx,
            components,
        )
        .await;
        rx
    }

    async fn pending_options(request_id: &str, key: &str) -> Option<Vec<UiOption>> {
        let channels = UI_RESPONSE_CHANNELS.lock().await;
        channels[request_id]
            .components
            .iter()
            .find(|c| c.key == key)
            .and_then(|c| c.options.clone())
    }

    const FILL: &str = r#"{"id":"mo","properties":{"options":[{"value":"MO-1","label":"MO-1"}]}}"#;

    /// The point: the prompt the submit is validated against, and the
    /// worker's own copy, both carry the run-time options, and the
    /// normalized payload comes back for the surfaces.
    #[tokio::test]
    async fn fill_patches_the_pending_prompt_and_the_live_copy() {
        let _rx = pending("set-props-fill", None, vec![select("mo")]).await;
        let mut live = vec![select("mo")];
        let emitted = apply_ui_set_properties(Some("set-props-fill"), FILL, &mut live)
            .await
            .unwrap()
            .expect("applied");
        assert_eq!(emitted.id, "mo");
        assert_eq!(
            serde_json::to_string(&emitted.properties).unwrap(),
            r#"{"options":[{"label":"MO-1","value":"MO-1"}]}"#
        );
        let opts = pending_options("set-props-fill", "mo").await.unwrap();
        assert_eq!(opts[0].value, "MO-1");
        assert_eq!(live[0].options.as_ref().unwrap()[0].value, "MO-1");
    }

    #[tokio::test]
    async fn unknown_key_is_refused() {
        let _rx = pending("set-props-unknown", None, vec![select("mo")]).await;
        let mut live = vec![select("mo")];
        let err = apply_ui_set_properties(
            Some("set-props-unknown"),
            r#"{"id":"nope","properties":{"label":"x"}}"#,
            &mut live,
        )
        .await
        .unwrap_err();
        assert!(err.contains("no component nope"), "{err}");
        assert!(err.contains("have: mo"), "{err}");
    }

    #[tokio::test]
    async fn closed_and_unread_properties_are_refused_untouched() {
        let _rx = pending("set-props-closed", None, vec![text_input("sn")]).await;
        let mut live = vec![text_input("sn")];
        // Closed to every type (layout): fails to parse as a patch.
        let err = apply_ui_set_properties(
            Some("set-props-closed"),
            r#"{"id":"sn","properties":{"rows":3}}"#,
            &mut live,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown field `rows`"), "{err}");
        // Settable somewhere, but not on a text_input.
        let err = apply_ui_set_properties(
            Some("set-props-closed"),
            r#"{"id":"sn","properties":{"prefix":"B-","options":[]}}"#,
            &mut live,
        )
        .await
        .unwrap_err();
        assert!(err.contains("has no 'options' property"), "{err}");
        assert_eq!(live[0].prefix.as_deref(), Some("A-"), "refusal is atomic");
        let channels = UI_RESPONSE_CHANNELS.lock().await;
        assert_eq!(
            channels["set-props-closed"].components[0].prefix.as_deref(),
            Some("A-")
        );
    }

    /// Operator answered before the phase got round to `ui.set`: the
    /// prompt is gone, and so is anything to update.
    #[tokio::test]
    async fn update_after_the_prompt_resolved_is_a_no_op() {
        let mut live = vec![select("mo")];
        let out = apply_ui_set_properties(Some("set-props-ghost"), FILL, &mut live)
            .await
            .unwrap();
        assert!(out.is_none());
        assert!(live[0].options.is_none());
    }

    /// Each slot's phase has its own prompt; patching one slot's leaves
    /// its sibling with the YAML screen.
    #[tokio::test]
    async fn slot_scoped_update_touches_only_its_prompt() {
        let _a = pending("set-props-slot-a", Some("A"), vec![select("mo")]).await;
        let _b = pending("set-props-slot-b", Some("B"), vec![select("mo")]).await;
        let mut live_a = vec![select("mo")];
        apply_ui_set_properties(Some("set-props-slot-a"), FILL, &mut live_a)
            .await
            .unwrap()
            .expect("applied");
        assert!(pending_options("set-props-slot-a", "mo").await.is_some());
        assert!(pending_options("set-props-slot-b", "mo").await.is_none());
    }

    /// A display-only screen has no pending prompt; the worker's copy
    /// still follows the phase.
    #[tokio::test]
    async fn display_only_screen_patches_the_live_copy() {
        let mut live = vec![UiComponent {
            key: "status".into(),
            ..UiComponent::new(ComponentType::Text)
        }];
        let out = apply_ui_set_properties(
            None,
            r#"{"id":"status","properties":{"label":"Fetching orders…"}}"#,
            &mut live,
        )
        .await
        .unwrap();
        assert!(out.is_some());
        assert_eq!(live[0].label.as_deref(), Some("Fetching orders…"));
    }

    /// The worker's live copy is what composes bound affixes at submit:
    /// a prefix set at run time is the one recorded, not the YAML's.
    #[tokio::test]
    async fn run_time_prefix_reaches_the_recorded_bound_value() {
        let component = UiComponent {
            bind: Some("measurements.sn".into()),
            ..text_input("sn")
        };
        let _rx = pending("set-props-affix", None, vec![component.clone()]).await;
        let mut live = vec![component];
        apply_ui_set_properties(
            Some("set-props-affix"),
            r#"{"id":"sn","properties":{"prefix":"B-"}}"#,
            &mut live,
        )
        .await
        .unwrap()
        .expect("applied");

        let mut values = HashMap::new();
        values.insert("sn".to_string(), "1".to_string());
        values.insert(
            "__bound_measurements__".to_string(),
            r#"{"sn":"1"}"#.to_string(),
        );
        let (_, bound) = super::extract_bound_measurements(&values, &live).expect("bound");
        assert_eq!(bound["sn"], serde_json::json!("B-1"));
    }

    /// The spec Python checks against is the engine's allow-list, not a
    /// second list.
    #[test]
    fn component_specs_carry_the_derived_allow_list() {
        let specs = ui_component_specs(&[select("mo"), text_input("sn")]);
        assert_eq!(specs[0].component_type, "select");
        assert_eq!(
            specs[0].settable,
            ["label", "description", "placeholder", "options"]
        );
        assert!(specs[1].settable.iter().any(|p| p == "pattern"));
        assert!(!specs[1].settable.iter().any(|p| p == "options"));
    }

    /// The option caps Python refuses at the call are the ones the engine
    /// enforces; a drift would turn a `ui.set` back into a silent refusal.
    #[test]
    fn python_option_caps_match_the_protocol() {
        let source = include_str!("../../python/tp_worker.py");
        let read = |name: &str| -> usize {
            let line = source
                .lines()
                .find(|l| l.starts_with(&format!("{name} = ")))
                .unwrap_or_else(|| panic!("tp_worker.py declares {name}"));
            line.split('=').nth(1).unwrap().trim().parse().unwrap()
        };
        assert_eq!(
            read("_MAX_OPTION_TEXT_CHARS"),
            station_protocol::properties::MAX_OPTION_TEXT_CHARS
        );
        assert_eq!(
            read("_MAX_OPTIONS_WITH_IMAGES"),
            station_protocol::properties::MAX_OPTIONS_WITH_IMAGES
        );
    }

    /// The Python worker keeps a shape rule per settable property (how to
    /// check the value before sending). Hold it against the Rust
    /// allow-list so neither side can grow a property the other does not
    /// know.
    #[test]
    fn python_shape_rules_match_the_settable_properties() {
        let source = include_str!("../../python/tp_worker.py");
        let start = source
            .find("_SETTABLE_SHAPES = {")
            .expect("tp_worker.py declares _SETTABLE_SHAPES");
        let body = &source[start..];
        let end = body.find("\n}").expect("dict closes");
        let mut python: Vec<&str> = body[..end]
            .lines()
            .skip(1)
            .filter_map(|l| l.trim().split(':').next())
            .map(|k| k.trim().trim_matches('"'))
            .filter(|k| !k.is_empty())
            .collect();
        python.sort_unstable();
        let mut rust: Vec<&str> = station_protocol::UNIVERSAL_SETTABLE_PROPERTIES
            .iter()
            .chain(station_protocol::SETTABLE_TYPE_SPECIFIC_FIELDS)
            .copied()
            .collect();
        rust.sort_unstable();
        assert_eq!(python, rust);
    }
}
