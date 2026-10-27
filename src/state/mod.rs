use crate::job::{Job, JobResult};
use crate::procedure::schema::{PhaseStage, StageScope};
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

mod worker_state;
pub use worker_state::WorkerStateTracker;

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub phase_key: String,
    pub phase_name: String,
    pub function: String,
    pub slot_id: Option<String>,
    pub dependency_id: Uuid,
    /// Stage of the phase. An interrupt must not kill a teardown that
    /// is already running (`interrupt_running_jobs`).
    pub stage_scope: StageScope,
    /// The sequence calls the phase came through (see `Job`).
    pub sequence_path: Vec<String>,
    pub sequence_stages: Vec<PhaseStage>,
    /// Attempt number (0 = first). A result recorded on the job's behalf
    /// (a teardown cut by the shutdown cap, an interruption, a late
    /// cancel) must carry it, or it ties with the earlier attempt and the
    /// final outcome is picked at random.
    pub retry_count: usize,
}

impl JobInfo {
    pub fn from_job(job: &Job) -> Self {
        Self {
            phase_key: job.phase_key.clone(),
            phase_name: job.phase_name.clone(),
            function: job.function.clone(),
            slot_id: job.slot_id.clone(),
            dependency_id: job.dependency_id,
            stage_scope: job.stage_scope,
            sequence_path: job.sequence_path.clone(),
            sequence_stages: job.sequence_stages.clone(),
            retry_count: job.retry_count,
        }
    }

    /// Teardown work at any level: the procedure's teardown stages or a
    /// sequence's teardown phase (`Job::is_teardown_work`, same rule).
    pub fn is_teardown(&self) -> bool {
        crate::job::is_teardown_work(self.stage_scope, &self.sequence_stages)
    }

    /// The phase sits under the sequence call at `path`.
    pub fn in_sequence_scope(&self, path: &[String]) -> bool {
        crate::job::path_has_prefix(&self.sequence_path, path)
    }
}

/// The scope a sequence call runs in: one per slot for a per-slot call,
/// one with no slot for a call at execution scope. Keyed by the slot and
/// the call's path joined by `/`.
pub type SequenceScope = (Option<String>, String);

/// Information about a pending delayed retry task
#[derive(Debug)]
pub struct PendingDelayedRetry {
    pub handle: tokio::task::JoinHandle<()>,
    pub phase_key: String,
    pub phase_name: String,
    pub function: String,
    pub slot_id: Option<String>,
    pub job_id: Uuid,
    pub dependency_id: Uuid,
    /// Attempt number of the retry that was waiting to run.
    pub retry_count: usize,
    /// Stage of the phase. Teardown phases retry too, during a stop
    /// included.
    pub stage_scope: StageScope,
    /// The sequence calls the phase came through, so a sequence
    /// cancellation aborts its pending retries too.
    pub sequence_path: Vec<String>,
}

impl PendingDelayedRetry {
    fn job_info(&self) -> JobInfo {
        JobInfo {
            phase_key: self.phase_key.clone(),
            phase_name: self.phase_name.clone(),
            function: self.function.clone(),
            slot_id: self.slot_id.clone(),
            dependency_id: self.dependency_id,
            stage_scope: self.stage_scope,
            sequence_path: self.sequence_path.clone(),
            sequence_stages: Vec::new(),
            retry_count: self.retry_count,
        }
    }

    fn in_sequence_scope(&self, path: &[String]) -> bool {
        crate::job::path_has_prefix(&self.sequence_path, path)
    }
}

/// Who interrupted a run that did not reach its last phase. The canonical
/// explanation for the flag/cause pair; the other doc blocks point here.
///
/// `shutdown_requested` alone once carried five meanings (operator Stop,
/// operator Kill, the CLI's graceful cancel, a plug init failure, and the
/// engine stopping itself after a failed phase). The aggregation could not
/// tell them apart, so a failing unit under the default
/// `on_first_failure: stop` uploaded as `ABORTED`; the first fix then
/// guessed the cause inside `cancel_all_jobs` and an operator-cancelled run
/// uploaded as `PASS` (TP-957).
///
/// - `PhaseFailure`: the sequence stopping itself after a phase failed, or a
///   failed setup. Not an abort: the run outcome must be `Fail`.
/// - `Operator`: a human or supervisor stopping the run from outside, kill
///   button graceful or forced. That is what `ABORTED` means.
/// - `InitFailure`: a plug that could not initialize. `init_error` is set
///   alongside and wins the aggregation; this only keeps the cause honest.
///
/// Set through `request_shutdown` wherever someone actually asks for a
/// stop. `Orchestrator::shutdown()`, the end-of-run teardown the CLI runs
/// after every run, raises the bare flag without a cause on purpose. A
/// raised flag with no cause reads as an interruption, never as a PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownCause {
    Operator,
    PhaseFailure,
    InitFailure,
}

/// A slot cut short before its last phase. The per-slot twin of the
/// `shutdown_requested` / `shutdown_cause` pair: presence in
/// `slot_stops` is the flag, `cause` reads like `shutdown_cause`.
///
/// Stop scope follows job scope. A job with `slot_id = Some(s)` that
/// stops (`on_first_failure: stop`, `then: {…: stop}`, `phase.stop()`,
/// Error, Timeout) cancels slot `s` only and records it here; a shared
/// job (SetupAll, TeardownAll) or an operator stops the execution, which
/// marks every slot. A bool would not do: after a SetupAll failure a
/// slot's own results are only SKIPs and would aggregate to PASS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotStop {
    pub cause: Option<ShutdownCause>,
    /// Quoted on the cancelled UI phases of that slot, so the operator
    /// reads why their prompt vanished.
    pub reason: String,
}

/// What a cancellation removed from the run: queued jobs, and delayed
/// retries that were waiting to be re-enqueued. Both need a Skipped
/// event from the caller.
#[derive(Debug, Default)]
pub struct CancelledWork {
    pub jobs: Vec<Job>,
    pub retries: Vec<PendingDelayedRetry>,
}

impl CancelledWork {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty() && self.retries.is_empty()
    }
}

/// Centralized state for the orchestrator to reduce lock complexity
///
/// Lock ordering convention:
/// 1. OrchestratorState (this struct)
/// 2. ResourceManager (if needed)
/// 3. Individual Workers (if needed)
///
/// Never acquire locks in reverse order to prevent deadlocks
#[derive(Debug)]
pub struct OrchestratorState {
    pub job_queue: VecDeque<Job>,
    pub completed_jobs: HashSet<Uuid>,
    pub job_results: HashMap<Uuid, JobResult>,
    pub job_info: HashMap<Uuid, JobInfo>,
    pub worker_state: WorkerStateTracker,
    pub total_jobs_submitted: usize, // Track the initial total job count (not repeats)
    pub original_jobs_completed: usize, // Track completed original jobs (not repeats)
    pub job_to_slot: HashMap<Uuid, String>, // Map job IDs to slot IDs
    pub slot_jobs: HashMap<String, HashSet<Uuid>>, // Map slot IDs to their job IDs
    pub shutdown_requested: bool,    // Flag to signal shutdown
    pub shutdown_cause: Option<ShutdownCause>, // Who raised it, see `ShutdownCause`
    /// An operator stop that landed once only teardown work was left: it
    /// cut nothing the outcome is judged on, so a FAIL already recorded
    /// stands (`determine_aggregate_outcome`).
    pub stop_spared_main: bool,
    /// Main jobs taken off the queue whose plugs or worker are still
    /// starting: neither queued nor running yet, still main work a stop
    /// cuts.
    pub starting_main: HashSet<Uuid>,
    pub force_kill_requested: bool, // Flag to signal immediate force kill
    pub should_stop_on_first_failure: bool, // Stop execution on first phase failure
    pub pending_slot_jobs: Vec<(String, Vec<Job>)>, // For slot-first: remaining slots to process
    pub teardown_procedure_jobs: Vec<Job>, // Teardown procedure jobs to run after all slots
    pub pending_delayed_retry_handles: Vec<PendingDelayedRetry>, // Handles to spawned retry delay tasks with job info
    pub init_error: Option<String>, // Error that occurred during initialization (e.g., plug init failure)
    /// What made an `Operator` stop when it was no person at the run: a
    /// signal (`SIGTERM`), a console event. Named in the reason the cut
    /// phases carry.
    pub stop_source: Option<String>,
    /// Every slot the procedure was submitted with, in declaration order.
    /// `slot_jobs` only knows slots that have had a job enqueued, which
    /// under slot-first leaves the not-yet-started ones invisible.
    pub slots: Vec<String>,
    /// Slot-first: one slot runs at a time, its slot plugs opening when
    /// its turn comes.
    pub slot_first: bool,
    /// Slots cut short, see `SlotStop`. First writer wins, like
    /// `request_shutdown`.
    pub slot_stops: HashMap<String, SlotStop>,
    /// Slots that had a setup or main job picked for dispatch, which is
    /// also when their slot plugs get created. A slot outside this set
    /// has nothing for its TeardownEach to undo.
    pub started_slots: HashSet<String>,
    /// Any setup or main job, shared or not, was picked for dispatch.
    pub any_work_started: bool,
    /// Plug-scope operations (scope plug start and release) reserved in
    /// `total_jobs_submitted`, and how many of them were counted done.
    /// A scope whose plugs were never started emits neither, so the
    /// remainder is settled when the run ends (`settle_plug_scope_ops`).
    pub plug_scope_reserved: usize,
    pub plug_scope_counted: usize,
    /// Every job of every sequence scope, registered at submit for the
    /// whole run (deferred slot-first jobs included) so a scope with
    /// nothing left to run is told apart from one not started yet. A job
    /// under `a/b` is registered under `a` and `a/b`. A call whose phases
    /// are all disabled is registered with no job: it settles as Skip.
    pub sequence_jobs: HashMap<SequenceScope, HashSet<Uuid>>,
    /// Sequence scopes cut short by a failing phase of theirs (or of a
    /// nested call), the per-sequence twin of `slot_stops`.
    pub sequence_stops: HashMap<SequenceScope, SlotStop>,
    /// Scopes whose outcome was computed and announced, so a scope
    /// settles once.
    pub settled_sequences: HashMap<SequenceScope, crate::job::Outcome>,
    /// Sequence scopes a job has started in (every enclosing call of a
    /// running job). A cancellation keeps a sequence's teardown only for
    /// an entered scope: a call that never began has nothing to tear
    /// down, and running its teardown would undo a setup that never ran.
    pub entered_sequences: HashSet<SequenceScope>,
}

/// A skipped phase's `error`: why it never ran, from the cause that cut
/// it (`Stopped by operator` → `Not run: stopped by operator`). A cause
/// opening on a name (`PSU failed to open`) keeps its case.
pub fn not_run(reason: &str) -> String {
    let mut chars = reason.chars();
    let lowered = match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => first
            .to_lowercase()
            .chain(reason[first.len_utf8()..].chars())
            .collect(),
        _ => reason.to_string(),
    };
    format!("Not run: {lowered}")
}

/// Teardown work a cancellation keeps queued: the procedure's own teardown
/// stages always, a sequence's teardown only once that sequence was entered
/// (a job of it started). A call that never began has nothing to tear down,
/// and running its teardown would undo a setup that never ran.
///
/// `from_depth` restricts the question to the calls at that depth or deeper
/// (1 = outermost call), for a cancellation of one sequence scope; 0 asks
/// about any level, for a slot or execution cancellation. The teardown a
/// job belongs to is the shallowest enclosing call whose stage it is in:
/// the whole teardown of an entered call runs, nested calls included.
pub fn keeps_on_cancel(entered: &HashSet<SequenceScope>, job: &Job, from_depth: usize) -> bool {
    if from_depth == 0
        && matches!(
            job.stage_scope,
            StageScope::TeardownEach | StageScope::TeardownAll
        )
    {
        return true;
    }
    let from = from_depth.saturating_sub(1);
    let Some(level) = job
        .sequence_stages
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, stage)| **stage == PhaseStage::Teardown)
        .map(|(i, _)| i)
    else {
        return false;
    };
    let scope: SequenceScope = (job.slot_id.clone(), job.sequence_path[..=level].join("/"));
    entered.contains(&scope)
}

impl OrchestratorState {
    pub fn new(num_workers: usize) -> Self {
        Self {
            job_queue: VecDeque::new(),
            completed_jobs: HashSet::new(),
            job_results: HashMap::new(),
            job_info: HashMap::new(),
            worker_state: WorkerStateTracker::new(num_workers),
            total_jobs_submitted: 0,
            original_jobs_completed: 0,
            job_to_slot: HashMap::new(),
            slot_jobs: HashMap::new(),
            shutdown_requested: false,
            shutdown_cause: None,
            stop_spared_main: false,
            starting_main: HashSet::new(),
            force_kill_requested: false,
            should_stop_on_first_failure: false,
            pending_slot_jobs: Vec::new(),
            teardown_procedure_jobs: Vec::new(),
            pending_delayed_retry_handles: Vec::new(),
            init_error: None,
            stop_source: None,
            slots: Vec::new(),
            slot_first: false,
            slot_stops: HashMap::new(),
            started_slots: HashSet::new(),
            any_work_started: false,
            plug_scope_reserved: 0,
            plug_scope_counted: 0,
            sequence_jobs: HashMap::new(),
            sequence_stops: HashMap::new(),
            settled_sequences: HashMap::new(),
            entered_sequences: HashSet::new(),
        }
    }

    /// Reserve `n` plug-scope operations in the run's progress total.
    pub fn reserve_plug_scope_ops(&mut self, n: usize) {
        self.plug_scope_reserved += n;
        self.total_jobs_submitted += n;
    }

    /// Count one plug-scope operation done. Capped at the reservation so
    /// progress never passes its total.
    pub fn count_plug_scope_op(&mut self) {
        if self.plug_scope_counted < self.plug_scope_reserved {
            self.plug_scope_counted += 1;
            self.original_jobs_completed += 1;
        }
    }

    /// Count the reserved plug-scope operations that never happened (a
    /// scope whose plugs were never started has nothing to start or
    /// release) so the run's progress closes. Only once no plug scope can
    /// still start or release.
    pub fn settle_plug_scope_ops(&mut self) {
        let remainder = self
            .plug_scope_reserved
            .saturating_sub(self.plug_scope_counted);
        self.plug_scope_counted += remainder;
        self.original_jobs_completed += remainder;
    }

    /// Record that `job` is being dispatched. Teardown jobs do not start
    /// a slot: they only undo what setup and main did.
    pub fn note_dispatched(&mut self, job: &Job) {
        if job.is_teardown() {
            return;
        }
        self.any_work_started = true;
        if let Some(slot_id) = &job.slot_id {
            self.started_slots.insert(slot_id.clone());
        }
    }

    /// A teardown job with nothing to undo: a TeardownEach of a slot that
    /// never started, or a TeardownAll of a run where nothing started. Its
    /// plugs were never created, so running it fails on them; it is
    /// skipped like the TeardownEach of a slot deferred under slot-first.
    pub fn teardown_has_nothing_to_undo(&self, job: &Job) -> bool {
        match job.stage_scope {
            StageScope::TeardownEach => job
                .slot_id
                .as_deref()
                .is_some_and(|slot| !self.started_slots.contains(slot)),
            StageScope::TeardownAll => !self.any_work_started,
            _ => false,
        }
    }

    /// Register the run's jobs under every sequence scope they belong to,
    /// and every call as a scope (with no job when none of its phases runs,
    /// so it still settles). Called once at submit with every job of the
    /// run, before any is enqueued.
    pub fn register_sequence_jobs(
        &mut self,
        jobs: &[Job],
        sequences: &[crate::procedure::schema::SequenceInstance],
        slots: &[String],
    ) {
        for seq in sequences {
            let path = seq.path_str();
            // A call made at execution scope runs once with no slot; any
            // other runs once per slot. A nested call carries no `scope:`
            // of its own (only a call written in the procedure's setup or
            // teardown may), so it takes its root call's: otherwise it
            // would open a phantom per-slot scope next to the shared one,
            // settle twice, and the upload would carry a duplicate path.
            let root_scope = sequences
                .iter()
                .find(|s| s.path.len() == 1 && s.path[0] == seq.path[0])
                .and_then(|s| s.scope);
            if root_scope == Some(crate::procedure::schema::Scope::Execution) {
                self.sequence_jobs.entry((None, path)).or_default();
            } else {
                for slot in slots {
                    self.sequence_jobs
                        .entry((Some(slot.clone()), path.clone()))
                        .or_default();
                }
            }
        }
        for job in jobs {
            self.register_sequence_job(job);
        }
    }

    fn register_sequence_job(&mut self, job: &Job) {
        for depth in 1..=job.sequence_path.len() {
            let path = job.sequence_path[..depth].join("/");
            self.sequence_jobs
                .entry((job.slot_id.clone(), path))
                .or_default()
                .insert(job.id);
        }
    }

    /// A job of a sequence scope is done when it completed, or when it was
    /// an attempt that got retried: `record_retry_attempt` keeps its
    /// result without entering `completed_jobs`, and the retry is a job of
    /// the scope in its own right (`enqueue_retry_job`). Without the second
    /// half, a phase retried twice leaves one attempt "open" for good and
    /// its call only settles at the end of the run, too late to stop
    /// anything.
    pub fn sequence_job_done(&self, id: &Uuid) -> bool {
        self.completed_jobs.contains(id) || self.job_results.contains_key(id)
    }

    /// Whether every registered job of the scope is done. `None` when the
    /// scope was never registered (a call that does not run in that slot).
    pub fn sequence_complete(&self, scope: &SequenceScope) -> Option<bool> {
        let jobs = self.sequence_jobs.get(scope)?;
        let all_done = jobs.iter().all(|id| self.sequence_job_done(id));
        let retry_pending = self
            .pending_delayed_retry_handles
            .iter()
            .any(|p| p.slot_id == scope.0 && p.in_sequence_scope(&split_path(&scope.1)));
        Some(all_done && !retry_pending)
    }

    /// A stop that lands while a call waits out a retry delay: no job of
    /// the call is queued or running at that moment, so nothing cancelled
    /// or interrupted would mark the call as cut, and it would settle on
    /// its failed attempt alone. Record the stop on every scope a pending
    /// retry sits in, before the handles are taken.
    pub fn mark_pending_retry_scopes_stopped(&mut self, reason: &str) {
        let cause = self.shutdown_cause;
        let scopes: Vec<SequenceScope> = self
            .pending_delayed_retry_handles
            .iter()
            .flat_map(|p| {
                (1..=p.sequence_path.len())
                    .map(move |depth| (p.slot_id.clone(), p.sequence_path[..depth].join("/")))
            })
            .collect();
        for scope in scopes {
            self.mark_sequence_stopped(scope, cause, reason);
        }
    }

    /// Record a sequence scope as cut short. First writer wins.
    pub fn mark_sequence_stopped(
        &mut self,
        scope: SequenceScope,
        cause: Option<ShutdownCause>,
        reason: &str,
    ) {
        self.sequence_stops
            .entry(scope)
            .or_insert_with(|| SlotStop {
                cause,
                reason: reason.to_string(),
            });
    }

    /// Cancel one sequence scope's remaining work: its queued jobs that
    /// are not teardown work within it, and its delayed retries. The
    /// scope's own teardown phases (and those of the calls nested in it)
    /// stay queued and run; the rest of the slot is untouched. Records the
    /// scope as stopped with `cause`.
    pub fn cancel_sequence_jobs(
        &mut self,
        slot_id: Option<&str>,
        path: &[String],
        cause: Option<ShutdownCause>,
        reason: &str,
    ) -> CancelledWork {
        let mut cancelled = CancelledWork::default();
        let depth = path.len();
        let entered = &self.entered_sequences;
        self.job_queue.retain(|job| {
            if job.slot_id.as_deref() == slot_id
                && job.in_sequence_scope(path)
                && !keeps_on_cancel(entered, job, depth)
            {
                cancelled.jobs.push(job.clone());
                false
            } else {
                true
            }
        });
        for job in &cancelled.jobs {
            self.complete_as_cancelled(job.id, JobInfo::from_job(job), job.retry_count);
        }
        cancelled.retries = self.abort_pending_retries(|p| {
            p.slot_id.as_deref() == slot_id && p.in_sequence_scope(path)
        });
        if !cancelled.is_empty() {
            log::info!(
                "Cancelling {} jobs and {} pending retries of sequence {} ({}): {}",
                cancelled.jobs.len(),
                cancelled.retries.len(),
                path.join("/"),
                slot_id.unwrap_or("<shared>"),
                reason
            );
        }
        self.mark_sequence_stopped((slot_id.map(str::to_string), path.join("/")), cause, reason);
        cancelled
    }

    /// Whether a sequence scope was cut short and by whom, as
    /// `(stopped, cause)` for `determine_aggregate_outcome`: its own stop
    /// first, else the stop of its slot or of the execution when that stop
    /// found work of the scope outstanding (a cancelled job of the scope).
    pub fn sequence_stop_state(
        &self,
        scope: &SequenceScope,
        had_cancelled_work: bool,
    ) -> (bool, Option<ShutdownCause>) {
        if let Some(stop) = self.sequence_stops.get(scope) {
            return (true, stop.cause);
        }
        if had_cancelled_work {
            if let Some(slot) = &scope.0 {
                if let Some(stop) = self.slot_stops.get(slot) {
                    return (true, stop.cause);
                }
            }
            if self.shutdown_requested {
                return (true, self.shutdown_cause);
            }
        }
        (false, None)
    }

    /// Raise the shutdown flag with its cause. First cause wins: an
    /// operator kill that landed first stays the reason, whatever the
    /// engine does afterwards to wind down. See `ShutdownCause`.
    ///
    /// An operator's stop marks the slots it cuts as it lands: a phase in
    /// flight keeps its own verdict when it returns, and a slot whose last
    /// phase then finishes would look complete.
    pub fn request_shutdown(&mut self, cause: ShutdownCause) {
        let first = !self.shutdown_requested;
        // The flag is lowered while the teardown runs; the cause is not:
        // it alone says whether this is the first stop.
        let first_cause = self.shutdown_cause.is_none();
        self.shutdown_requested = true;
        self.shutdown_cause.get_or_insert(cause);
        if first_cause && cause == ShutdownCause::Operator {
            self.stop_spared_main = !self.main_work_outstanding();
        }
        if first && cause == ShutdownCause::Operator {
            let reason = self.stop_reason();
            self.mark_outstanding_slots_stopped(&reason);
            // The same for the sequence calls under way: one whose last
            // phase then finishes was still cut, it settles STOP.
            let running: Vec<SequenceScope> = self
                .entered_sequences
                .iter()
                .filter(|scope| self.sequence_complete(scope) == Some(false))
                .cloned()
                .collect();
            for scope in running {
                self.mark_sequence_stopped(scope, Some(cause), &reason);
            }
        }
    }

    /// Check if execution is complete. Work deferred under slot-first
    /// (`pending_slot_jobs`, `teardown_procedure_jobs`) counts: the queue
    /// is empty between two slots, and a slot stop no longer raises the
    /// flag that used to end the loop there.
    pub fn is_complete(&self) -> bool {
        (self.job_queue.is_empty()
            && self.worker_state.count_busy() == 0
            && self.pending_delayed_retry_handles.is_empty()
            && self.pending_slot_jobs.is_empty()
            && self.teardown_procedure_jobs.is_empty())
            || self.shutdown_requested
    }

    /// Clean up finished delayed retry task handles
    pub fn cleanup_finished_retry_handles(&mut self) {
        self.pending_delayed_retry_handles
            .retain(|pending| !pending.handle.is_finished());
    }

    /// Get the next ready job from the queue
    pub fn pop_ready_job(&mut self, check_deps: impl Fn(&Job) -> bool) -> Option<Job> {
        let mut checked_jobs = Vec::new();
        let mut ready_job = None;

        // Find first job with satisfied dependencies
        while let Some(job) = self.job_queue.pop_front() {
            if job.dependencies_satisfied(&self.completed_jobs) && check_deps(&job) {
                ready_job = Some(job);
                break;
            } else {
                checked_jobs.push(job);
            }
        }

        // Put non-ready jobs back
        for job in checked_jobs.into_iter().rev() {
            self.job_queue.push_front(job);
        }

        ready_job
    }

    /// Mark a job as active
    pub fn mark_job_active(&mut self, job_id: Uuid, worker_id: usize) -> Result<(), String> {
        self.starting_main.remove(&job_id);
        self.worker_state.assign_job(worker_id, job_id)
    }

    /// Whether a teardown phase of `stage` other than `except` is still
    /// queued, running, waiting on a delayed retry, or deferred. For
    /// `TeardownEach` only `slot_id`'s phases count. The scope's plugs
    /// stay up until this is false: the next teardown phase of the scope
    /// (or the retry of this one) needs them.
    pub fn teardown_outstanding(
        &self,
        stage: StageScope,
        slot_id: Option<&str>,
        except: Uuid,
    ) -> bool {
        let in_scope = |job_stage: StageScope, job_slot: Option<&str>| {
            job_stage == stage && (stage == StageScope::TeardownAll || job_slot == slot_id)
        };
        let job_in_scope =
            |j: &Job| j.id != except && in_scope(j.stage_scope, j.slot_id.as_deref());
        self.job_queue.iter().any(job_in_scope)
            || self.teardown_procedure_jobs.iter().any(job_in_scope)
            || self
                .pending_slot_jobs
                .iter()
                .flat_map(|(_, jobs)| jobs)
                .any(job_in_scope)
            || self
                .pending_delayed_retry_handles
                .iter()
                .any(|p| in_scope(p.stage_scope, p.slot_id.as_deref()))
            || (0..self.worker_state.num_workers()).any(|w| {
                self.worker_state.get_worker_job(w).is_some_and(|id| {
                    id != except
                        && self
                            .job_info
                            .get(&id)
                            .is_some_and(|i| in_scope(i.stage_scope, i.slot_id.as_deref()))
                })
            })
    }

    /// Complete a job and resolve its dependency_id to unblock dependents
    pub fn complete_job(&mut self, job_id: Uuid, result: JobResult) {
        self.completed_jobs.insert(job_id);
        // Also insert dependency_id so dependents waiting on the original UUID get unblocked
        if let Some(info) = self.job_info.get(&job_id) {
            if info.dependency_id != job_id {
                self.completed_jobs.insert(info.dependency_id);
            }
        }
        self.job_results.insert(job_id, result);
        self.worker_state.release_by_job(&job_id);
    }

    /// Record a retry attempt without satisfying dependencies.
    /// Stores result and releases the worker, but does NOT add to completed_jobs.
    pub fn record_retry_attempt(&mut self, job_id: Uuid, result: JobResult) {
        self.job_results.insert(job_id, result);
        self.worker_state.release_by_job(&job_id);
    }

    /// Complete an original (non-repeat) job
    pub fn complete_original_job(&mut self, job_id: Uuid, result: JobResult) {
        self.complete_job(job_id, result);
        self.original_jobs_completed += 1;
    }

    pub fn complete_job_with_info(&mut self, job_id: Uuid, job: &Job, result: JobResult) {
        self.job_info.insert(job_id, JobInfo::from_job(job));
        self.complete_original_job(job_id, result);
    }

    /// Record a slot as cut short. First writer wins: a slot that
    /// stopped on its own failure keeps `PhaseFailure` when the operator
    /// later stops the whole run.
    pub fn mark_slot_stopped(&mut self, slot_id: &str, cause: Option<ShutdownCause>, reason: &str) {
        self.slot_stops
            .entry(slot_id.to_string())
            .or_insert_with(|| SlotStop {
                cause,
                reason: reason.to_string(),
            });
    }

    pub fn is_slot_stopped(&self, slot_id: &str) -> bool {
        self.slot_stops.contains_key(slot_id)
    }

    /// The slot a job belongs to is stopping, or the whole execution is.
    /// Shared jobs only ever stop with the execution.
    pub fn is_stopping_for(&self, slot_id: Option<&str>) -> bool {
        self.shutdown_requested || slot_id.is_some_and(|s| self.is_slot_stopped(s))
    }

    /// Why a shutdown cuts the work it cuts, by its cause: an operator's
    /// stop, a failure that stops the run, a run that could not start.
    /// The interrupted phases carry it as is, the skipped ones through
    /// [`not_run`].
    pub fn stop_reason(&self) -> String {
        match self.shutdown_cause {
            Some(ShutdownCause::InitFailure) => match &self.init_error {
                Some(e) => e.clone(),
                None => "The run could not start".to_string(),
            },
            Some(ShutdownCause::PhaseFailure) => "The run stopped on a failure".to_string(),
            Some(ShutdownCause::Operator) | None => match &self.stop_source {
                Some(source) => format!("Stopped by {source}"),
                None => "Stopped by operator".to_string(),
            },
        }
    }

    /// Why a force kill cut the work: the signal behind it, else the
    /// operator.
    pub fn kill_reason(&self) -> String {
        match &self.stop_source {
            Some(source) => format!("Killed by {source}"),
            None => "Killed by operator".to_string(),
        }
    }

    /// Mark every slot that still has work as stopped, with the cause the
    /// execution stop recorded. Called by the operator paths before they
    /// drain the queue: once running jobs are completed as interrupted
    /// and queued ones as skipped, nothing in the results can tell a slot
    /// that finished from one that was cut, and the cut one would read
    /// PASS.
    pub fn mark_outstanding_slots_stopped(&mut self, reason: &str) {
        let cause = self.shutdown_cause;
        let shared_outstanding = self.shared_work_outstanding();
        let slots: Vec<String> = self
            .slots
            .iter()
            .filter(|s| shared_outstanding || self.slot_has_outstanding_work(s))
            .cloned()
            .collect();
        for slot in slots {
            self.mark_slot_stopped(&slot, cause, reason);
        }
    }

    /// Whether any work other than teardown is still queued, starting,
    /// running, waiting on a delayed retry, or deferred under slot-first.
    fn main_work_outstanding(&self) -> bool {
        let running_main = (0..self.worker_state.num_workers()).any(|w| {
            self.worker_state
                .get_worker_job(w)
                .and_then(|id| self.job_info.get(&id))
                .is_some_and(|info| !info.is_teardown())
        });
        running_main
            || !self.starting_main.is_empty()
            || self.job_queue.iter().any(|j| !j.is_teardown_work())
            || !self.pending_delayed_retry_handles.is_empty()
            || !self.pending_slot_jobs.is_empty()
    }

    /// Whether a shared stage (SetupAll, TeardownAll) is still queued,
    /// running or deferred. Shared work belongs to every slot's outcome,
    /// so while it is outstanding no slot has finished.
    pub fn shared_work_outstanding(&self) -> bool {
        !self.teardown_procedure_jobs.is_empty()
            || self.job_queue.iter().any(|j| j.slot_id.is_none())
            || (0..self.worker_state.num_workers()).any(|w| {
                self.worker_state
                    .get_worker_job(w)
                    .is_some_and(|id| !self.job_to_slot.contains_key(&id))
            })
    }

    /// Whether anything of the slot is still queued, running, waiting on a
    /// delayed retry, or deferred under slot-first. Its own work only; see
    /// `shared_work_outstanding` for the stages every slot depends on.
    pub fn slot_has_outstanding_work(&self, slot_id: &str) -> bool {
        let is_slot = |s: &Option<String>| s.as_deref() == Some(slot_id);
        self.job_queue.iter().any(|j| is_slot(&j.slot_id))
            || self
                .pending_delayed_retry_handles
                .iter()
                .any(|p| is_slot(&p.slot_id))
            || self.pending_slot_jobs.iter().any(|(s, _)| s == slot_id)
            || (0..self.worker_state.num_workers()).any(|w| {
                self.worker_state.get_worker_job(w).is_some_and(|id| {
                    self.job_to_slot.get(&id).map(String::as_str) == Some(slot_id)
                })
            })
    }

    pub(crate) fn skip_result(retry_count: usize) -> JobResult {
        JobResult {
            phase_result: crate::job::PhaseResult::Skip,
            phase_outcome: crate::job::Outcome::Skip,
            next_action: None, // Will be computed in completion handler
            timeout_secs: None,
            timeout_ms: None,
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: chrono::Utc::now(),
            completed_at: chrono::Utc::now(),
            resource_metrics: None,
            unit: None,
            input_unit_info: None,
            retry_count,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        }
    }

    /// Record a job that will never run as Skip so it appears in the
    /// report and unblocks its dependents. Counts toward the original job
    /// total even for a retry attempt: `record_retry_attempt` does not
    /// count the earlier attempts, and no attempt of this phase instance
    /// ever will be.
    fn complete_as_cancelled(&mut self, job_id: Uuid, info: JobInfo, retry_count: usize) {
        self.job_info.insert(job_id, info);
        self.complete_original_job(job_id, Self::skip_result(retry_count));
    }

    /// Abort the delayed retries matching `keep_out`, resolving their
    /// dependency so dependents (the slot's TeardownEach) are not blocked
    /// forever. Without this a retried main phase re-enqueues after the
    /// slot was cancelled and runs after TeardownEach destroyed its plugs.
    fn abort_pending_retries(
        &mut self,
        mut matches: impl FnMut(&PendingDelayedRetry) -> bool,
    ) -> Vec<PendingDelayedRetry> {
        let (aborted, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pending_delayed_retry_handles)
                .into_iter()
                .partition(|p| matches(p));
        self.pending_delayed_retry_handles = kept;
        for pending in &aborted {
            pending.handle.abort();
            // The attempt that asked for the retry is in `job_results`
            // already (`record_retry_attempt`); the retry itself never ran.
            self.complete_as_cancelled(pending.job_id, pending.job_info(), pending.retry_count);
        }
        aborted
    }

    /// Teardown work still to run. After an operator stop these are the
    /// jobs `shutdown()` runs on fresh workers, and they need the
    /// scope plugs the run created. Under slot-first the shared
    /// teardown waits in `teardown_procedure_jobs`, not in the queue.
    pub fn has_queued_teardown(&self) -> bool {
        !self.teardown_procedure_jobs.is_empty()
            || self
                .job_queue
                .iter()
                .any(|job| keeps_on_cancel(&self.entered_sequences, job, 0))
            || self.has_pending_teardown_retry()
    }

    /// Abort the delayed retries `matches` selects because the run is
    /// shutting down. Each records Skip (the retry never ran), resolves its
    /// dependency and counts toward progress once, like a cancelled job.
    pub fn abort_retries_for_shutdown(
        &mut self,
        matches: impl FnMut(&PendingDelayedRetry) -> bool,
    ) -> Vec<PendingDelayedRetry> {
        self.abort_pending_retries(matches)
    }

    /// A teardown phase that failed is waiting out its `retry.delay`.
    pub fn has_pending_teardown_retry(&self) -> bool {
        self.pending_delayed_retry_handles
            .iter()
            .any(|p| Self::is_teardown_stage(p.stage_scope))
    }

    /// Abort the delayed teardown retries still waiting (the shutdown cap
    /// hit before they ran) and record each as an error with `message`.
    pub fn fail_pending_teardown_retries(&mut self, message: &str) -> Vec<PendingDelayedRetry> {
        let (failed, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pending_delayed_retry_handles)
                .into_iter()
                .partition(|p| Self::is_teardown_stage(p.stage_scope));
        self.pending_delayed_retry_handles = kept;
        for pending in &failed {
            pending.handle.abort();
            self.job_info.insert(pending.job_id, pending.job_info());
            let mut result = JobResult::new_error(message.to_string());
            result.retry_count = pending.retry_count;
            self.complete_original_job(pending.job_id, result);
        }
        failed
    }

    fn is_teardown_stage(stage: StageScope) -> bool {
        matches!(stage, StageScope::TeardownEach | StageScope::TeardownAll)
    }

    fn is_teardown(job: &Job) -> bool {
        job.is_teardown()
    }

    /// Record that a job of these sequence scopes started: the call and
    /// every call enclosing it are entered from now on.
    pub fn mark_sequence_entered(&mut self, job: &Job) {
        for depth in 1..=job.sequence_path.len() {
            self.entered_sequences
                .insert((job.slot_id.clone(), job.sequence_path[..depth].join("/")));
        }
    }

    /// Cancel one slot's remaining work: its queued non-teardown jobs and
    /// its delayed retries. Its TeardownEach stays queued and runs, unless
    /// the slot never started (`teardown_has_nothing_to_undo`); the
    /// other slots and the shared stages are untouched. Records the slot
    /// as stopped with `cause`, see `SlotStop`.
    pub fn cancel_slot_jobs(
        &mut self,
        slot_id: &str,
        cause: Option<ShutdownCause>,
        reason: &str,
    ) -> CancelledWork {
        let mut cancelled = CancelledWork::default();

        let slot_started = self.started_slots.contains(slot_id);
        let entered = &self.entered_sequences;
        self.job_queue.retain(|job| {
            // Kept: the procedure's teardown of a slot that started (a
            // slot never started has nothing to undo), and the teardown of
            // the sequence calls this slot entered.
            let keep =
                keeps_on_cancel(entered, job, 0) && !(Self::is_teardown(job) && !slot_started);
            if job.slot_id.as_deref() == Some(slot_id) && !keep {
                cancelled.jobs.push(job.clone());
                false
            } else {
                true
            }
        });

        for job in &cancelled.jobs {
            self.complete_as_cancelled(job.id, JobInfo::from_job(job), job.retry_count);
        }

        // A teardown retry is cleanup work like the queued teardown: it
        // still runs.
        cancelled.retries = self.abort_pending_retries(|p| {
            p.slot_id.as_deref() == Some(slot_id) && !Self::is_teardown_stage(p.stage_scope)
        });

        if !cancelled.is_empty() {
            log::info!(
                "Cancelling {} jobs and {} pending retries of slot {}: {}",
                cancelled.jobs.len(),
                cancelled.retries.len(),
                slot_id,
                reason
            );
        }

        self.mark_slot_stopped(slot_id, cause, reason);

        cancelled
    }

    /// Cancel every slot's remaining work (a shared stage failed, or a
    /// stop was asked for the whole execution). Teardown phases are kept:
    /// they must run for cleanup, except where there is nothing to clean
    /// up (`teardown_has_nothing_to_undo`). Slots deferred under
    /// slot-first are cancelled whole, TeardownEach included: none of
    /// their setup ran, so there is nothing of theirs to tear down.
    ///
    /// `cause` is what the CALLER knows about why the run is stopping. It is
    /// deliberately not inferred here: a cancellation triggered by a phase
    /// that failed is a `PhaseFailure`, but the same code path also runs
    /// when a phase merely *finished* under an operator stop — and that must
    /// not be relabelled as a failure. Pass `None` when the cause was
    /// already recorded by whoever raised `shutdown_requested`.
    pub fn cancel_all_jobs(&mut self, reason: &str, cause: Option<ShutdownCause>) -> CancelledWork {
        let mut cancelled = CancelledWork::default();

        // Drain the queue but preserve teardown phases
        let all_jobs: Vec<Job> = self.job_queue.drain(..).collect();
        for job in all_jobs {
            if keeps_on_cancel(&self.entered_sequences, &job, 0)
                && !self.teardown_has_nothing_to_undo(&job)
            {
                self.job_queue.push_back(job);
            } else {
                cancelled.jobs.push(job);
            }
        }

        // Deferred slots were never enqueued, so they are not in the
        // submitted total yet; count them in before counting them done.
        for (_, jobs) in self.pending_slot_jobs.drain(..) {
            self.total_jobs_submitted += jobs.len();
            cancelled.jobs.extend(jobs);
        }

        for job in &cancelled.jobs {
            self.complete_as_cancelled(job.id, JobInfo::from_job(job), job.retry_count);
        }

        cancelled.retries = self.abort_pending_retries(|p| !Self::is_teardown_stage(p.stage_scope));

        if !cancelled.is_empty() {
            log::info!(
                "Cancelling {} jobs and {} pending retries: {}",
                cancelled.jobs.len(),
                cancelled.retries.len(),
                reason
            );
        }

        let slots = self.slots.clone();
        for slot in &slots {
            self.mark_slot_stopped(slot, cause, reason);
        }

        // Only set shutdown flag if no teardown phases remain
        // (teardown phases must still run for cleanup)
        if !self.has_queued_teardown() {
            match cause {
                Some(cause) => self.request_shutdown(cause),
                // Caller has no cause of its own (a phase merely finished
                // under an existing stop): raise the flag, leave the cause
                // to whoever recorded it. `None` aggregates as Stop.
                None => self.shutdown_requested = true,
            }
        }

        cancelled
    }

    /// Add a job to the queue
    pub fn enqueue_job(&mut self, job: Job) {
        self.requeue_job(job);
        self.total_jobs_submitted += 1;
    }

    /// Put back a job the submitted total already counts (the teardown
    /// `shutdown()` drained from the queue and runs on its own workers).
    pub fn requeue_job(&mut self, job: Job) {
        if let Some(slot_id) = job.slot_id.clone() {
            self.job_to_slot.insert(job.id, slot_id.clone());
            self.slot_jobs.entry(slot_id).or_default().insert(job.id);
        }
        self.job_queue.push_back(job);
    }

    /// Add a retry job to the front of the queue without incrementing total count
    pub fn enqueue_retry_job(&mut self, job: Job) {
        // A retry is a new job of the same scopes: without this the scope
        // would read complete while its retry is still queued.
        self.register_sequence_job(&job);
        // Track job-slot relationship
        let slot_id = job.slot_id.clone();
        let job_id = job.id;

        if let Some(slot_id_str) = slot_id {
            self.job_to_slot.insert(job_id, slot_id_str.clone());
            self.slot_jobs
                .entry(slot_id_str)
                .or_default()
                .insert(job_id);
        }

        self.job_queue.push_front(job);
        // Don't increment total_jobs_submitted for retries
    }

    /// Check if a slot is complete and queue next slot if using slot-first execution
    pub fn check_and_queue_next_slot(&mut self) -> bool {
        // Don't queue new slots if shutdown was requested
        if self.shutdown_requested {
            return false;
        }

        // Check if there are pending slots to queue
        if self.pending_slot_jobs.is_empty() {
            // Check if we need to queue teardown procedure jobs
            // Must also check pending_delayed_retry_handles to avoid starting teardown
            // while a retry is still waiting to be enqueued
            if !self.teardown_procedure_jobs.is_empty()
                && self.job_queue.is_empty()
                && self.worker_state.count_busy() == 0
                && self.pending_delayed_retry_handles.is_empty()
            {
                log::trace!("📋 All slots complete, enqueueing teardown procedure phases");
                // Collect jobs first to avoid borrow issues
                let teardown_jobs: Vec<Job> = self.teardown_procedure_jobs.drain(..).collect();
                for job in teardown_jobs {
                    self.enqueue_job(job);
                }
                return true;
            }
            return false;
        }

        // Check if current slot work is complete (no jobs in queue, no busy workers,
        // and no pending delayed retries)
        if self.job_queue.is_empty()
            && self.worker_state.count_busy() == 0
            && self.pending_delayed_retry_handles.is_empty()
        {
            // Queue the next slot's jobs
            if !self.pending_slot_jobs.is_empty() {
                let (slot_id, jobs) = self.pending_slot_jobs.remove(0);
                log::trace!("📦 Slot complete, starting next slot: {}", slot_id);
                for job in jobs {
                    self.enqueue_job(job);
                }
                return true;
            }
        }

        false
    }
}

/// `a/b/c` as its keys.
fn split_path(path: &str) -> Vec<String> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').map(str::to_string).collect()
    }
}

#[cfg(test)]
mod stop_scope_tests {
    use super::*;
    use crate::procedure::schema::StageScope;

    use crate::test_support::job;

    /// First cause wins. An operator kill that landed first must not be
    /// relabelled by the engine winding down afterwards; the first fix's
    /// blocker was exactly a later writer overwriting the cause.
    #[test]
    fn request_shutdown_keeps_the_first_cause() {
        let mut state = OrchestratorState::new(1);
        state.request_shutdown(ShutdownCause::Operator);
        state.request_shutdown(ShutdownCause::PhaseFailure);
        assert!(state.shutdown_requested);
        assert_eq!(state.shutdown_cause, Some(ShutdownCause::Operator));
    }

    /// `cancel_all_jobs` raises the flag and records the caller's cause
    /// only once no teardown phase is left to run. With a teardown phase
    /// queued, neither moves: the aggregation must then rely on the
    /// results themselves (`then: {pass: stop}` case).
    #[test]
    fn cancel_all_jobs_records_the_cause_only_when_the_queue_drains() {
        let mut with_teardown = OrchestratorState::new(1);
        // A phase ran: the shared teardown has something to undo.
        with_teardown.note_dispatched(&job(StageScope::Main));
        with_teardown.enqueue_job(job(StageScope::Main));
        with_teardown.enqueue_job(job(StageScope::TeardownAll));
        let cancelled = with_teardown.cancel_all_jobs("x", Some(ShutdownCause::PhaseFailure));
        assert_eq!(cancelled.jobs.len(), 1, "only the main phase is cancelled");
        assert!(!with_teardown.shutdown_requested);
        assert_eq!(with_teardown.shutdown_cause, None);

        let mut no_teardown = OrchestratorState::new(1);
        no_teardown.enqueue_job(job(StageScope::Main));
        no_teardown.cancel_all_jobs("x", Some(ShutdownCause::PhaseFailure));
        assert!(no_teardown.shutdown_requested);
        assert_eq!(
            no_teardown.shutdown_cause,
            Some(ShutdownCause::PhaseFailure)
        );

        // A caller with no cause raises the flag and leaves the cause alone.
        let mut no_cause = OrchestratorState::new(1);
        no_cause.enqueue_job(job(StageScope::Main));
        no_cause.cancel_all_jobs("x", None);
        assert!(no_cause.shutdown_requested);
        assert_eq!(no_cause.shutdown_cause, None);
    }

    /// Stop scope = job scope. Cancelling slot A leaves slot B's queued
    /// main phase alone, keeps A's TeardownEach, aborts A's delayed retry
    /// (resolving its dependency), and records A as stopped with the
    /// caller's cause. The execution flag stays down.
    #[tokio::test]
    async fn cancel_slot_jobs_touches_one_slot_only() {
        let mut state = OrchestratorState::new(1);
        state.slots = vec!["a".into(), "b".into()];
        // Both slots ran a first phase: their teardowns have something to undo.
        state.note_dispatched(&slot_job(StageScope::Main, "a"));
        state.note_dispatched(&slot_job(StageScope::Main, "b"));
        state.enqueue_job(slot_job(StageScope::Main, "a"));
        state.enqueue_job(slot_job(StageScope::Main, "b"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "b"));
        state.enqueue_job(job(StageScope::TeardownAll));

        let retry_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let handle =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .pending_delayed_retry_handles
            .push(PendingDelayedRetry {
                handle,
                phase_key: "k".into(),
                phase_name: "Phase".into(),
                function: "f".into(),
                slot_id: Some("a".into()),
                job_id: retry_id,
                dependency_id: dep_id,
                sequence_path: Vec::new(),
                retry_count: 2,
                stage_scope: StageScope::Main,
            });

        let cancelled = state.cancel_slot_jobs("a", Some(ShutdownCause::PhaseFailure), "a failed");

        assert_eq!(cancelled.jobs.len(), 1, "only A's main phase is cancelled");
        assert_eq!(cancelled.retries.len(), 1, "A's pending retry is aborted");
        assert!(
            cancelled.retries[0].handle.is_finished() || {
                tokio::task::yield_now().await;
                cancelled.retries[0].handle.is_finished()
            }
        );
        assert!(
            state.completed_jobs.contains(&dep_id),
            "aborted retry unblocks dependents"
        );
        assert_eq!(
            state.job_results[&retry_id].retry_count, 2,
            "the skip records the attempt that was pending"
        );
        assert!(state.pending_delayed_retry_handles.is_empty());
        assert_eq!(
            state.job_queue.len(),
            4,
            "B's main, both TeardownEach, TeardownAll stay"
        );
        assert!(state.job_queue.iter().any(
            |j| j.slot_id.as_deref() == Some("b") && matches!(j.stage_scope, StageScope::Main)
        ));
        assert!(state.is_slot_stopped("a"));
        assert!(!state.is_slot_stopped("b"));
        assert_eq!(
            state.slot_stops["a"].cause,
            Some(ShutdownCause::PhaseFailure)
        );
        assert!(
            !state.shutdown_requested,
            "a slot stop never raises the execution flag"
        );
        assert!(state.is_stopping_for(Some("a")));
        assert!(!state.is_stopping_for(Some("b")));
        assert!(!state.is_stopping_for(None));
    }

    /// An execution-wide cancel marks every slot, drains the slots still
    /// deferred under slot-first (their TeardownEach included: nothing
    /// of theirs was set up), and leaves the flag down while a shared
    /// teardown is still to run.
    #[test]
    fn cancel_all_jobs_marks_every_slot_and_drains_deferred_slots() {
        let mut state = OrchestratorState::new(1);
        state.slots = vec!["a".into(), "b".into()];
        state.note_dispatched(&slot_job(StageScope::Main, "a"));
        state.enqueue_job(slot_job(StageScope::Main, "a"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        state.pending_slot_jobs = vec![(
            "b".into(),
            vec![
                slot_job(StageScope::Main, "b"),
                slot_job(StageScope::TeardownEach, "b"),
            ],
        )];
        state.teardown_procedure_jobs = vec![job(StageScope::TeardownAll)];

        let cancelled = state.cancel_all_jobs("setup failed", Some(ShutdownCause::PhaseFailure));

        assert_eq!(
            cancelled.jobs.len(),
            3,
            "A's main plus both of B's deferred jobs"
        );
        assert_eq!(
            state.total_jobs_submitted, 4,
            "B's deferred jobs join the submitted total"
        );
        assert_eq!(state.original_jobs_completed, 3);
        assert!(state.pending_slot_jobs.is_empty());
        assert_eq!(state.job_queue.len(), 1, "A's TeardownEach stays");
        assert!(state.is_slot_stopped("a") && state.is_slot_stopped("b"));
        assert_eq!(
            state.slot_stops["b"].cause,
            Some(ShutdownCause::PhaseFailure)
        );
        assert!(
            !state.shutdown_requested,
            "TeardownEach and TeardownAll still have to run"
        );
        assert!(!state.is_complete());
    }

    /// The operator path: a slot that finished every phase keeps no
    /// outstanding work and must not be marked; one with a job queued is.
    #[test]
    fn mark_outstanding_slots_stopped_spares_finished_slots() {
        let mut state = OrchestratorState::new(1);
        state.slots = vec!["done".into(), "live".into(), "never".into(), "empty".into()];
        let finished = slot_job(StageScope::Main, "done");
        let finished_id = finished.id;
        state.enqueue_job(finished);
        state.enqueue_job(slot_job(StageScope::Main, "live"));
        state.pending_slot_jobs = vec![("never".into(), vec![slot_job(StageScope::Main, "never")])];
        state
            .job_info
            .insert(finished_id, JobInfo::from_job(&state.job_queue[0]));
        let popped = state.job_queue.pop_front().unwrap();
        state.complete_job(popped.id, skipped());

        state.request_shutdown(ShutdownCause::Operator);
        state.mark_outstanding_slots_stopped("Stopped by operator");

        assert!(!state.is_slot_stopped("done"));
        assert_eq!(
            state.slot_stops["live"].cause,
            Some(ShutdownCause::Operator)
        );
        assert!(
            state.is_slot_stopped("never"),
            "a deferred slot that never started was cut too"
        );
        assert!(
            !state.is_slot_stopped("empty"),
            "a slot with no job of its own has nothing to cut"
        );

        // A shared teardown still queued: every slot, the empty one
        // included, was cut before its execution finished.
        let mut with_shared = OrchestratorState::new(1);
        with_shared.slots = vec!["empty".into()];
        with_shared.enqueue_job(job(StageScope::TeardownAll));
        with_shared.request_shutdown(ShutdownCause::Operator);
        with_shared.mark_outstanding_slots_stopped("Stopped by operator");
        assert!(with_shared.is_slot_stopped("empty"));
    }

    /// Slot-first parks the shared teardown outside the queue; the
    /// plug-keeping decision at the end of `execute_all` has to see it.
    #[test]
    fn has_queued_teardown_sees_the_parked_shared_teardown() {
        let mut state = OrchestratorState::new(1);
        assert!(!state.has_queued_teardown());
        state
            .teardown_procedure_jobs
            .push(job(StageScope::TeardownAll));
        assert!(state.has_queued_teardown());

        let mut queued = OrchestratorState::new(1);
        queued.enqueue_job(job(StageScope::Main));
        assert!(!queued.has_queued_teardown());
        queued.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        assert!(queued.has_queued_teardown());
    }

    fn skipped() -> JobResult {
        OrchestratorState::skip_result(0)
    }

    /// A slot the stop caught before any of its phases was dispatched has
    /// nothing to tear down: its TeardownEach is cancelled with the rest,
    /// whether the slot alone stops or the whole execution does. The
    /// shared teardown stays once anything ran, and goes when nothing did.
    #[test]
    fn teardown_of_work_that_never_started_is_cancelled() {
        let mut state = OrchestratorState::new(1);
        state.slots = vec!["a".into(), "b".into()];
        state.note_dispatched(&slot_job(StageScope::Main, "a"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        state.enqueue_job(slot_job(StageScope::Main, "b"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "b"));
        state.enqueue_job(job(StageScope::TeardownAll));
        let cancelled = state.cancel_slot_jobs("b", None, "stop b");
        assert_eq!(cancelled.jobs.len(), 2, "B's main and its TeardownEach");
        assert_eq!(
            state.job_queue.len(),
            2,
            "A's TeardownEach and the TeardownAll stay"
        );

        let mut state = OrchestratorState::new(1);
        state.slots = vec!["a".into(), "b".into()];
        state.note_dispatched(&slot_job(StageScope::Main, "a"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        state.enqueue_job(slot_job(StageScope::Main, "b"));
        state.enqueue_job(slot_job(StageScope::TeardownEach, "b"));
        state.enqueue_job(job(StageScope::TeardownAll));
        let cancelled = state.cancel_all_jobs("stop", Some(ShutdownCause::Operator));
        assert_eq!(cancelled.jobs.len(), 2, "B's main and its TeardownEach");
        assert_eq!(
            state.job_queue.len(),
            2,
            "A's TeardownEach and the TeardownAll stay"
        );
        assert_eq!(state.original_jobs_completed, 2);

        let mut nothing_ran = OrchestratorState::new(1);
        nothing_ran.slots = vec!["a".into()];
        nothing_ran.enqueue_job(slot_job(StageScope::TeardownEach, "a"));
        nothing_ran.enqueue_job(job(StageScope::TeardownAll));
        let cancelled = nothing_ran.cancel_all_jobs("stop", Some(ShutdownCause::Operator));
        assert_eq!(cancelled.jobs.len(), 2);
        assert!(nothing_ran.shutdown_requested, "no teardown left to run");
    }

    /// Plug-scope operations that never happened are counted when the run
    /// settles, and progress never passes its total.
    #[test]
    fn plug_scope_ops_settle_without_overcounting() {
        let mut state = OrchestratorState::new(1);
        state.reserve_plug_scope_ops(4);
        state.count_plug_scope_op();
        state.settle_plug_scope_ops();
        assert_eq!(state.original_jobs_completed, 4);
        state.count_plug_scope_op();
        state.settle_plug_scope_ops();
        assert_eq!(
            state.original_jobs_completed, 4,
            "nothing past the reservation"
        );
        assert_eq!(state.total_jobs_submitted, 4);
    }

    fn slot_job(scope: StageScope, slot: &str) -> Job {
        let mut j = job(scope);
        j.slot_id = Some(slot.to_string());
        j
    }
}
