//! Event emission for UI communication

use crate::event_sink::ExecutionEvent;
use crate::job::{Job, JobStatus, Outcome};
use crate::procedure::schema::ProcedureDefinition;
use crate::state::OrchestratorState;
use crate::{PlannedPhase, PlannedPlug};

use super::{ExecutionPlan, JobProgress, Orchestrator};

impl Orchestrator {
    pub(super) fn emit_job_progress(
        &self,
        job_id: String,
        original_job: &Job,
        status: JobStatus,
        outcome: Option<Outcome>,
        error: Option<String>,
        worker_id: Option<usize>,
    ) {
        let progress = JobProgress {
            job_id: job_id.clone(),
            slot_id: original_job.slot_id.clone(),
            phase_key: original_job.phase_key.clone(),
            phase_name: original_job.phase_name.clone(),
            stage_scope: original_job.stage_scope,
            status,
            worker_id,
            started_at: None,
            timeout_ms: original_job.timeout_ms,
            outcome,
            retry_count: original_job.retry_count,
            error: error.clone(),
        };
        self.event_sink.emit(&ExecutionEvent::JobProgress {
            job_id: progress.job_id,
            slot_id: progress.slot_id,
            phase_key: progress.phase_key,
            phase_name: progress.phase_name,
            stage_scope: progress.stage_scope,
            status: progress.status,
            worker_id: progress.worker_id,
            started_at: progress.started_at,
            timeout_ms: progress.timeout_ms,
            outcome: progress.outcome,
            retry_count: progress.retry_count,
            error: progress.error,
        });
    }

    pub(super) async fn emit_cancelled_jobs(
        &self,
        cancelled_jobs: &[Job],
        reason: &str,
        status: JobStatus,
        outcome: Outcome,
    ) {
        if !cancelled_jobs.is_empty() {
            for job in cancelled_jobs {
                self.emit_job_progress(
                    job.id.to_string(),
                    job,
                    status,
                    Some(outcome),
                    Some(reason.to_string()),
                    None,
                );
            }
        }
    }

    /// Skipped events for a cancellation: the queued jobs it removed and
    /// the delayed retries it aborted, which had no queued job to speak
    /// for them.
    pub(super) async fn emit_cancelled_work(
        &self,
        cancelled: &crate::state::CancelledWork,
        reason: &str,
    ) {
        self.emit_cancelled_jobs(&cancelled.jobs, reason, JobStatus::Skipped, Outcome::Skip)
            .await;
        for pending in &cancelled.retries {
            self.event_sink.emit(&ExecutionEvent::JobProgress {
                job_id: pending.job_id.to_string(),
                slot_id: pending.slot_id.clone(),
                phase_key: pending.phase_key.clone(),
                phase_name: pending.phase_name.clone(),
                stage_scope: pending.stage_scope,
                status: JobStatus::Skipped,
                worker_id: None,
                started_at: None,
                timeout_ms: None,
                outcome: Some(Outcome::Skip),
                retry_count: pending.retry_count,
                error: Some(format!("Retry cancelled: {reason}")),
            });
        }
    }

    pub(super) async fn emit_plug_scope_event(&self, status: &str) {
        if status == "pass" || status == "error" {
            self.state.write().await.count_plug_scope_op();
        }
    }

    /// `partial_main_set` / `partial_plugs`: a partial run's filters
    /// (Some only for partial runs). The plan must announce exactly
    /// what will execute — a phase listed here but never given a job
    /// gets "flushed" with the run outcome by UI reducers at
    /// run_complete, painting phases that never ran as PASS; a plug
    /// listed but never started sits "pending" forever.
    pub(super) async fn emit_execution_plan(
        &self,
        procedure: &ProcedureDefinition,
        state: &OrchestratorState,
        slots: &[String],
        partial_main_set: Option<&std::collections::HashSet<String>>,
        partial_plugs: Option<&[String]>,
    ) {
        let mut phases = Vec::new();
        for (stage_scope, phase) in procedure.get_all_phases_with_stage_scope() {
            if phase.should_skip() {
                continue;
            }
            if let Some(set) = partial_main_set {
                if matches!(stage_scope, crate::procedure::schema::StageScope::Main)
                    && !set.contains(&phase.key)
                {
                    continue;
                }
            }
            phases.push(PlannedPhase {
                phase_key: phase.key.clone(),
                phase_name: phase.name.clone(),
                stage_scope,
                sequence_path: phase.sequence_path.clone(),
            });
        }

        // Station plugs ride in the shared (`plugs_all`) bucket — one
        // instance shared by every slot, exactly like execution scope from the
        // plan's point of view — but keep their own scope string so the
        // UI can label them.
        let (plugs_all, plugs_each): (Vec<_>, Vec<_>) = procedure
            .plugs
            .iter()
            .filter(|p| {
                partial_plugs
                    .map(|keys| keys.contains(&p.key))
                    .unwrap_or(true)
            })
            .partition(|p| p.scope != crate::procedure::schema::Scope::Slot);

        // Report the EFFECTIVE scope, not the declared one: without a
        // station plug host the plug was downgraded to execution scope at
        // init (see initialization.rs), and every subsequent
        // plug_status event carries "execution" — the plan must agree or the
        // operator UI seeds a card whose label contradicts its events.
        let station_effective = self.station_plug_host.is_some();
        let plugs_all: Vec<PlannedPlug> = plugs_all
            .into_iter()
            .map(|p| PlannedPlug {
                plug_key: p.key.clone(),
                plug_name: p.name.clone(),
                scope: if p.scope == crate::procedure::schema::Scope::Station && station_effective {
                    "station".to_string()
                } else {
                    "execution".to_string()
                },
            })
            .collect();

        let plugs_each: Vec<PlannedPlug> = plugs_each
            .into_iter()
            .map(|p| PlannedPlug {
                plug_key: p.key.clone(),
                plug_name: p.name.clone(),
                scope: "slot".to_string(),
            })
            .collect();

        // Every call, including one whose phases are all disabled or
        // filtered out of a partial run: it still settles (as Skip), and
        // consumers want the group to exist before its event arrives.
        let sequences: Vec<crate::event_sink::PlannedSequence> = procedure
            .sequences
            .iter()
            .map(|s| crate::event_sink::PlannedSequence {
                path: s.path.clone(),
                key: s.key().to_string(),
                name: s.name.clone(),
                source: s.source.clone(),
                stage: s.stage,
                scope: s.scope.map(|sc| sc.as_str().to_string()),
            })
            .collect();

        let plan = ExecutionPlan {
            phases: phases.clone(),
            plugs_all: plugs_all.clone(),
            plugs_each: plugs_each.clone(),
            slots: slots.to_vec(),
            total_expected_jobs: state.total_jobs_submitted as u32,
            sequences: sequences.clone(),
        };

        self.event_sink.emit(&ExecutionEvent::Plan {
            phases,
            plugs_all,
            plugs_each,
            slots: plan.slots,
            total_expected_jobs: plan.total_expected_jobs,
            sequences,
        });
    }
}
