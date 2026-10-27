use serde::{Deserialize, Serialize};

/// Budget-driven degradation for events too large for the Centrifugo
/// frame limit. See `shrink.rs` for the failure mode it prevents.
mod shrink;
pub use shrink::MAX_EVENT_BYTES;

/// Operator-facing messages derived from a field's regex (TP-760).
/// Lives in this crate — not the engine — because it is the single
/// source of truth for every surface that shows a validation message:
/// the engine and TUI call it natively, Studio's embedded runner
/// depends on this crate for it, and the browser surfaces receive its
/// output over the wire (`UiResponseRejected`, the studio-ws
/// validate request). No client re-implements any of it.
pub mod pattern_messages;

/// Operator-response validation against a prompt's component spec —
/// the one validator behind every submit path. See the module docs.
pub mod validate;

/// Run-time property updates from a phase (`ui.set(key, options=…)`):
/// which properties a phase may change, and the one patch every surface
/// applies. See the module docs.
pub mod properties;
pub use properties::{
    UiPropertyPatch, UiSetPropertiesPayload, CLOSED_TYPE_SPECIFIC_FIELDS,
    SETTABLE_TYPE_SPECIFIC_FIELDS, SET_PROPERTIES_ACTION, UNIVERSAL_SETTABLE_PROPERTIES,
};

// ---------------------------------------------------------------------------
// Station events (CLI -> Dashboard via Centrifugo)
// ---------------------------------------------------------------------------

/// Events the CLI publishes: run-scoped ones in a `RunEnvelope` on
/// `station_run:<id>`, the rest bare on the channel `channel()` names.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StationEvent {
    // -- Station lifecycle --
    /// Hardware info, published on connect
    Hardware {
        installation_id: String,
        hostname: String,
        os: String,
        /// Platform identifier (os_arch) — linux_x86_64, linux_aarch64,
        /// macos_arm64, etc. Always sent.
        platform: String,
        mac_address: Option<String>,
        cli_version: String,
        /// How the station daemon is running: `"root_system"` when the
        /// CLI runs as root (installed as a system service), `"user"`
        /// otherwise. Drives the dashboard's run-mode-derived UI (e.g.
        /// disabling the kiosk toggle, since a root system service has
        /// no graphical session). Optional + `default` so older CLIs
        /// that omit it deserialize cleanly; consumers treat a missing
        /// value as unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_mode: Option<String>,
    },
    /// System telemetry, published periodically
    Telemetry {
        installation_id: String,
        cpu_percent: f32,
        memory_mb: f32,
        disk_free_mb: f32,
        temperature_c: Option<f32>,
    },
    /// The daemon is building or rebuilding a procedure's Python
    /// environment (`uv venv`, `uv pip install` or `uv sync`), or just
    /// finished. Emitted by the Studio daemon only, whichever caller
    /// asked for the venv: the Run button, the assistant's `python_env`
    /// or `plug_methods`, the plug debugger. A first install or a
    /// rebuild after a dependency edit takes a minute with no other
    /// sign of life, and every other venv-touching request waits on
    /// it. Not a run event: no `execution_id`, the run-state reducer
    /// ignores it, the page shows it as a notice.
    VenvBootstrap {
        stage: VenvBootstrapStage,
        /// Absolute venv directory, Windows verbatim prefix stripped.
        venv: String,
        /// Which dependency file drives the build.
        source: StudioDepsSource,
        /// `true` when an existing venv is rebuilt because its
        /// dependency file changed, `false` on a first build.
        rebuilding: bool,
        /// On `finished`: why the build failed, the tail of uv's
        /// output included. Absent on success and on `started`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // -- Deployment --
    /// Pull started for a procedure
    DeploymentPullStarted {
        installation_id: String,
        procedure_id: String,
        deployment_id: String,
    },
    /// Pull completed successfully
    DeploymentPullCompleted {
        installation_id: String,
        procedure_id: String,
        deployment_id: String,
        file_count: u32,
    },
    /// Pull failed
    DeploymentPullFailed {
        installation_id: String,
        procedure_id: String,
        deployment_id: String,
        error: String,
        /// Machine-readable tag when the failure was a recognised OS error
        /// (`crate::os_error::kind` in the CLI): `filesystem_corrupt`,
        /// `disk_full`, `access_denied`, ... `None` for everything else.
        ///
        /// Separate from `error` so the fleet can be grouped on it without
        /// parsing a localized sentence back apart. Optional so a station
        /// running an older CLI keeps validating.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<String>,
    },
    /// Stale deployment removed from station
    DeploymentRemoved {
        installation_id: String,
        procedure_id: String,
        deployment_id: String,
    },
    /// New deployment added to the station's local set. Emitted by
    /// the CLI's pull loop when a procedure appears in the active
    /// pull response that wasn't in the prior local list. Lets a
    /// live-connected operator-ui kiosk update its procedure picker
    /// without a reload — the alternative was a frozen `HelloPayload`
    /// snapshot that only refreshed on reconnect.
    DeploymentAdded {
        installation_id: String,
        procedure_id: String,
        procedure_name: String,
        deployment_id: String,
    },

    // -- Run execution --
    /// Run started with phase plan + slot plan.
    RunStarted {
        procedure_id: String,
        /// Human-readable procedure name (e.g. "Battery FVT"). Lets
        /// consumers render the run header without a reverse-lookup
        /// against a station-procedures list — older clients had no
        /// way to learn the name when the run's procedure wasn't in
        /// the locally-known set (e.g. a station running a procedure
        /// that hasn't been linked via `station_procedures` yet, or
        /// the kiosk SPA which only knows the in-flight run's id).
        /// Defaulted empty for back-compat with old emitters.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        procedure_name: String,
        /// Per-run identity, minted at `run::start()` (UUID). Stamped on
        /// every lifecycle event for this run (`RunStarted`, `RunComplete`,
        /// `RunCrashed`) so consumers can drop stale terminals from a
        /// prior run that race a fresh `RunStarted` — operator presses
        /// Stop then New Run, the cancelled run's `RunComplete(ABORTED)`
        /// can land after the new run's `RunStarted`. Aliased from the
        /// old `run_name` field for back-compat with older emitters.
        #[serde(alias = "run_name")]
        execution_id: String,
        phases: Vec<PhasePlan>,
        /// Declared slot ids (e.g. `"slot_0"`). Empty for single-slot runs
        /// where `slot_id` on phase events is `None`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        slots: Vec<String>,
        /// Display names of the declared slots (`execution.slots[].name`),
        /// keyed by slot id. Empty when the procedure declares none.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        slot_names: std::collections::HashMap<String, String>,
        /// Multi-slot: the unit identified for each slot before the run
        /// started, keyed by slot id. Lets a consumer that joins mid-run
        /// (hydration replay) show every slot's serial without folding the
        /// pre-run `identify_resolved` events. Empty on single-slot runs,
        /// where `unit` carries the one unit.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        slot_units: std::collections::HashMap<String, UnitInfo>,
        /// Plug definitions declared by the procedure. Lets consumers
        /// pre-seed their plug state instead of materializing entries
        /// on the first `plug_status` / `plug_log` event — avoids
        /// ghost entries when events arrive with mismatched keys or
        /// out of order. Empty for procedures that declare no plugs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugs: Vec<PlugDefinition>,
        /// ISO 8601 timestamp at which the engine recorded the start.
        /// Lets UIs anchor their elapsed-time counter on the engine's
        /// clock instead of `Date.now()` at fold time, which drifts
        /// across a hydration replay.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Optional run id assigned at start (cloud-sync stations
        /// pre-mint one). Lets UIs link to the dashboard while the
        /// run is still in flight; before this field shipped, the
        /// run_id only arrived on `RunComplete`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        /// Deployment this run executes, when the procedure came from
        /// a pulled deployment (None for local-path runs). Lets remote
        /// UIs resolve relative image paths in UI components against
        /// the deployment's stored files — the dashboard serves them
        /// from the artifact bucket keyed by this id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deployment_id: Option<String>,
        /// The procedure's declared `version` (procedure.yaml), the one the
        /// uploaded run carries. None when the procedure declares none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_version: Option<String>,
        /// The procedure's `description` (procedure.yaml). None when empty.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_description: Option<String>,
        /// Unit metadata captured before the engine started. None
        /// for runs that prompt the operator at runtime — those
        /// arrive via `IdentifyRequest` / `IdentifyResolved`
        /// (`IdentifyResolved` always fires before `RunStarted` so
        /// the unit is also already on this field when the engine
        /// begins).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<UnitInfo>,
        /// Partial run: the main phase this run was narrowed to (the
        /// Builder's play button). Carried on the event so "Run again"
        /// can repeat the same partial run instead of silently
        /// escalating to the whole procedure. None for full runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_phase: Option<String>,
        /// The sequence calls of the procedure, parents before children.
        /// Empty for a procedure without `uses:`, absent from emitters that
        /// predate sequences.
        // Always sent, empty included, like every list here, so the
        // required TS type holds for this emitter. specta types no field of
        // an enum variant as optional (`procedure_version` above is the
        // same): a reader of an older station's event still defaults it.
        #[serde(default)]
        sequences: Vec<SequencePlan>,
        /// The procedure's run-level operator screen (root `ui.panel`),
        /// shown from this event to the verdict. Sent once, here: the
        /// pinned `run_started` replays it to a viewer that joins late.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panel: Option<UiPanel>,
    },
    /// A sequence call finished in one scope: every phase under it ran,
    /// skipped or was cancelled. One per call and per slot (no `slot_id`
    /// for a call at execution scope). `outcome` uses the run outcome
    /// words (`PASS`, `FAIL`, `ERROR`, `TIMEOUT`, `ABORTED`) plus `SKIP`
    /// when none of its phases ran.
    SequenceComplete {
        /// Instance keys from the root to the call; the `sequence_path` of
        /// its phases in `RunStarted.phases` starts with it.
        sequence_path: Vec<String>,
        key: String,
        name: String,
        /// The `uses:` reference as written.
        source: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        /// First start and last end among the call's phases that ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Phase execution began.
    PhaseStarted {
        phase_key: String,
        name: String,
        /// Slot this attempt belongs to. None for single-slot / shared
        /// lifecycle phases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        /// 1-indexed attempt number. Second attempt = 2 (first retry).
        #[serde(default = "one")]
        attempt: u32,
        /// Lifecycle stage: `setup_all` / `setup_each` / `main` /
        /// `teardown_each` / `teardown_all`. UIs render setup/teardown
        /// stages with a small prefix; without this field they had to
        /// look up the stage via the `RunStarted.phases` plan, which
        /// breaks for late-discovered phases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
        /// ISO 8601 attempt-start timestamp. Mirrors
        /// `PhaseComplete.started_at` so live UIs can render an
        /// elapsed-time stopwatch from the engine's clock.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Per-run identity from `RunStarted`. Lets reducers drop
        /// events from a cancelled prior run that share `phase_key`
        /// with the current run (same procedure → same phase keys).
        /// Optional for back-compat with older emitters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A single log line from a running phase. Streams live, unlike
    /// `PhaseComplete.logs` which only ships the batch when the phase
    /// terminates. UIs prepend new lines to the active phase's log
    /// view so operators see long phases unfold without waiting for
    /// completion. Subset of `PhaseLogLine` with the phase context
    /// attached.
    PhaseLog {
        phase_key: String,
        /// 1-indexed attempt this line belongs to.
        #[serde(default = "one")]
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        level: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Source filename the log line came from, relative to the
        /// procedure dir. Optional — some emitters (plug log
        /// forwarding) don't carry source context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A measurement value landed mid-phase. Lets UIs render the
    /// row immediately rather than waiting for `PhaseComplete`.
    /// Updates are idempotent: a later event for the same
    /// `(phase_key, attempt, name)` replaces the prior value.
    MeasurementUpdate {
        phase_key: String,
        #[serde(default = "one")]
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        measurement: RunMeasurement,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A phase attached a file or data blob. Streams live for YAML
    /// procedures (engine emits at attach time); OpenHTF connector
    /// emits a batch right after each phase since OpenHTF has no
    /// public on-attach hook. Either way the event lands before the
    /// terminal `RunComplete`.
    AttachmentAdded {
        phase_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        name: String,
        /// Local filesystem path or storage URL. Optional because
        /// `attach.data()` may stage in memory before persisting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mimetype: Option<String>,
        /// u32 caps at ~4 GB; large enough for any reasonable
        /// attachment, small enough to avoid BigInt on the TS side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u32>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Phase execution completed with results.
    PhaseComplete {
        phase_key: String,
        name: String,
        outcome: String,
        measurements: Vec<RunMeasurement>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        #[serde(default = "one")]
        attempt: u32,
        /// ISO 8601 timestamp when the phase attempt started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        /// ISO 8601 timestamp when the phase attempt ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<String>,
        /// Duration in milliseconds (ended_at - started_at). May be absent
        /// when the engine couldn't parse one of the timestamps. u32 caps at
        /// ~49 days — plenty for a phase; avoids BigInt on the TS side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u32>,
        /// Error diagnostic string, populated when the phase errored or a
        /// measurement validator failed decisively.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Captured stdout/stderr log lines for this attempt, in order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        logs: Vec<PhaseLogLine>,
        /// Per-run identity from `RunStarted`. Critical: a stray
        /// `PhaseComplete{outcome:FAIL}` from a cancelled prior run
        /// matching `phase_key` would otherwise stamp FAIL on the new
        /// run's same-named phase. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// Run-level metadata this attempt set (`run.metadata[...]`), each
        /// value as display text (JSON for a non-string). Later phases
        /// overwrite a key; consumers merge.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        run_metadata: std::collections::HashMap<String, String>,
    },
    /// Run execution completed
    RunComplete {
        outcome: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        /// Multi-slot procedures: each slot's own outcome (`slot_id` →
        /// outcome string), the outcome its uploaded run carries. Empty on
        /// single-slot runs, whose `outcome` already is the run's.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        slot_outcomes: std::collections::HashMap<String, String>,
        /// Matches the `execution_id` from `RunStarted`. Lets consumers
        /// drop a terminal event whose run is no longer the active one
        /// (e.g. a cancelled run's `RunComplete(ABORTED)` arriving after
        /// the operator already kicked off a fresh run). Optional for
        /// back-compat with older emitters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// ISO 8601 time the station ended the run, on the same clock as
        /// `RunStarted.timestamp`: the run's duration on every screen is
        /// the station's, not the time the event took to arrive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    /// Run uploaded to TofuPilot API. Emitted only after a successful
    /// `POST /api/v2/runs` — never on enqueue. Consumers can take this
    /// as the authoritative "run exists in the dashboard" signal and
    /// surface the dashboard link.
    RunUploaded {
        procedure_id: String,
        run_id: String,
        /// Execution this upload belongs to. Lets consumers drop a late
        /// `run_uploaded` from a previous run of the same procedure
        /// (upload finishing after the operator clicked Run again, or a
        /// queue retry) instead of stamping its dashboard link on the
        /// active run. Optional for back-compat with older emitters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// Slot whose run this is, on multi-slot procedures. None on a
        /// single-slot run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_key: Option<String>,
        /// Absolute URL the operator can hit to view the run in the
        /// dashboard. Built by the CLI from its `base_url` so the
        /// client doesn't have to guess between cloud and self-hosted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dashboard_url: Option<String>,
    },

    // -- Upload queue --
    //
    // Emitted by the CLI's background queue drain so operator UIs can
    // surface what's still in flight, what failed, and why. Each event
    // is keyed by `queue_id` (`<procedure_id>_<unix_millis>`) so
    // consumers maintain a single map.
    /// Run was queued for upload. Emitted once at enqueue time —
    /// always the first event for a given `queue_id`.
    RunUploadQueued {
        queue_id: String,
        procedure_id: String,
        outcome: String,
        /// Slot whose run this is, on multi-slot procedures.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial_number: Option<String>,
        attachment_count: u32,
        /// ISO-8601 wall-clock timestamp the run was enqueued.
        queued_at: String,
    },
    /// A new upload attempt is starting. Emitted before each retry so
    /// UIs can flip the row to "uploading" and show the attempt
    /// number.
    RunUploadStarted { queue_id: String, attempt: u32 },
    /// Upload finished successfully. Mirrors `RunUploaded` for the
    /// queue map; consumers may key off either.
    RunUploadSucceeded {
        queue_id: String,
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dashboard_url: Option<String>,
    },
    /// Upload attempt failed. Carries the classified error so UIs can
    /// distinguish "client error → don't retry" from "transient → will
    /// retry". `next_retry_at` is null for client errors (4xx) which
    /// the CLI parks until the operator manually retries.
    RunUploadFailed {
        queue_id: String,
        attempt: u32,
        /// One of `http_4xx` / `http_5xx` / `network` / `unknown`.
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        /// Server response body or transport-level error message,
        /// truncated to a sane length on the CLI side.
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_retry_at: Option<String>,
    },
    /// Entry removed from the queue. Either the operator dropped it
    /// (`reason = "manual"`) or the CLI gave up (`reason = "ttl"` /
    /// `reason = "invalid"`).
    RunUploadDropped { queue_id: String, reason: String },
    /// A single attachment finished uploading to storage. Emitted
    /// per-attachment from the upload queue once the server has minted an
    /// `upload_id` and the bytes are in object storage. Lets a remote
    /// operator-UI (which only ever saw a station-disk `path` on
    /// `AttachmentAdded`) resolve the attachment to a fetchable URL
    /// (`/api/attachments/{upload_id}`) and swap its pending placeholder
    /// for the real image. Correlated to the originating `AttachmentAdded`
    /// by `(phase_key, name)`: the queue carries the phase key on each
    /// attachment so the join is unique even when two phases attach the
    /// same file name. `phase_key` may be empty for legacy queue entries
    /// persisted before it was carried — consumers fall back to name.
    AttachmentUploaded {
        run_id: String,
        /// Phase the attachment belongs to. Matches
        /// `AttachmentAdded.phase_key`. Empty for legacy queue entries.
        #[serde(default)]
        phase_key: String,
        /// Matches `AttachmentAdded.name` for the same attachment.
        name: String,
        /// Server-minted upload id; the operator-UI builds
        /// `/api/attachments/{upload_id}` from it.
        upload_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mimetype: Option<String>,
    },
    /// Run failed to start, or aborted mid-flight before reaching a normal
    /// `RunComplete`. UIs should land on a terminal "errored" view that
    /// surfaces `error` to the operator. Always followed by a `RunComplete`
    /// with outcome `"ERROR"` so reducers that only key off completeness
    /// still terminate cleanly; consumers that distinguish "load failure"
    /// from "phase failure" should prefer this event's `error_kind`.
    RunCrashed {
        /// Procedure that failed. Stamped so UIs subscribed before any
        /// `RunStarted` (e.g. dashboard tab opened during station boot)
        /// can attribute the error.
        procedure_id: String,
        /// Operator-facing diagnostic. Multi-line is allowed; the UI
        /// renders it as monospace below the outcome banner.
        error: String,
        /// Coarse taxonomy. Lets UIs colour / group the screen without
        /// pattern-matching on free-form text. Open-set: unrecognised
        /// values render as a generic error.
        ///
        /// Today's values:
        ///   * `load_error`      — YAML / Python load-time failure (parse,
        ///     missing dep, validator).
        ///   * `init_error`      — Engine setup failure after load.
        ///   * `submit_error`    — Procedure submission rejected.
        ///   * `execution_error` — Engine crashed during execution.
        ///   * `subprocess_crash`— Python process died unexpectedly
        ///     (OpenHTF connector path).
        ///   * `not_configured`  — Required config or secrets have no value
        ///     on this station: the run was refused before identifying
        ///     the unit. `missing` names them.
        error_kind: String,
        /// Matches the `execution_id` from `RunStarted`. Lets consumers
        /// drop crash events that race ahead of a freshly-started run.
        /// Optional for back-compat with older emitters and for crashes
        /// that happen before any `RunStarted` has been emitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// ISO 8601 time of the crash on the station's clock: see
        /// `RunComplete.timestamp`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// `not_configured` only: the required values without one, so a
        /// UI can list them without parsing `error`. Names only, never a
        /// value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing: Option<Vec<MissingValue>>,
    },
    /// Execution progress snapshot
    RunProgress {
        completed: u32,
        failed: u32,
        running: u32,
        total: u32,
    },

    // -- Plugs --
    /// Plug lifecycle status change
    PlugStatus {
        plug_key: String,
        plug_name: String,
        stage: String,
        status: String,
        /// `"slot"` | `"execution"` | `"station"` — plug lifetime scope.
        /// Defaults empty for pre-scope emitters.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        scope: String,
        /// Slot this plug transition applies to. None for execution- and
        /// station-scope plugs (shared across slots).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Plug log line (stdout/stderr from plug lifecycle calls).
    PlugLog {
        plug_key: String,
        plug_name: String,
        level: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        /// Lifecycle stage at the time the line was emitted —
        /// `"setup"` / `"teardown"` / `"manual"`. None when the
        /// engine can't disambiguate (the plug subprocess streams
        /// across stages, so it's the consumer's job to bucket
        /// using the latest `plug_status.stage`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Source filename the line came from, relative to the procedure
        /// dir, as on `PhaseLog`. None for a plug's unstructured stderr.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },

    // -- Operator UI --
    /// Operator UI prompt requested
    UiRequest {
        request_id: String,
        phase_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        components: Option<Vec<UiComponent>>,
        requires_input: bool,
        /// Wall-clock the engine emitted this request (RFC 3339).
        /// Used by the UI to anchor the auto-submit countdown so a
        /// reconnect / hydration replay doesn't reset the window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// Engine job that opened this prompt, the same id its
        /// `UiUpdate`s carry. A retried phase keeps its `phase_key` but
        /// gets a new job, so reducers match an update to its prompt on
        /// this when both sides carry it, and fall back to `phase_key`
        /// (+ `slot_id`) for older emitters that omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job_id: Option<String>,
        /// The phase's custom screen (`ui.panel`), drawn in place of the
        /// component list by surfaces that render HTML. A skin over
        /// `components`, which stay the source of truth: surfaces that
        /// don't render panels (TUI, `--json`, older UIs) ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panel: Option<UiPanel>,
    },
    /// Operator UI display update. Emitted when Python code mutates a
    /// running prompt's component values via `ui.<key> = value`. Reducer
    /// finds the active `UiRequest` by `phase_key` (+ `slot_id` when
    /// present) and applies `data` per `action`:
    ///   * `set_value` — `data` is `{ id, value }`; mutate that
    ///     component's runtime value.
    ///
    /// Older engines (pre-runtime-update support) don't emit this.
    /// `data` is opaque JSON so future actions (`add_component`,
    /// `add_log`) extend without a wire break.
    UiUpdate {
        phase_key: String,
        action: String,
        /// JSON-encoded payload. Shape depends on `action`. Optional so
        /// pre-payload engines that emitted `UiUpdate` without a body
        /// still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        /// Worker job id. Lets multi-slot runs disambiguate when the
        /// same phase fans out across slots. Optional for legacy emits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job_id: Option<String>,
        /// Slot the update targets. Optional for single-slot runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        /// Per-run identity from `RunStarted`. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },

    /// An operator submission (`StationCommand::UiResponse`) failed
    /// validation against the prompt's component spec. The prompt stays
    /// pending — the waiting phase never sees the values — and the
    /// submitting surface renders `errors` on the still-open form.
    /// Covers phase prompts and identify-unit prompts alike (both park
    /// on the same response channel). Correlation is by `request_id`
    /// only: a rejection for a prompt the client no longer shows is
    /// dropped on the floor.
    ///
    /// Messages come from `validate::validate_response` — the single
    /// validator — so every surface shows identical wording. Clients
    /// must not pre-validate and re-word.
    UiResponseRejected {
        request_id: String,
        /// Component key → operator-facing message.
        errors: std::collections::HashMap<String, String>,
    },
    /// The counterpart: the submission passed validation and resolved
    /// the waiting phase — or the prompt was no longer pending (already
    /// answered elsewhere, timed out, run moved on), which the client
    /// treats identically: close the form. Submitting surfaces stay
    /// pessimistic — they keep the form open in its submitting state
    /// until one of Accepted / Rejected arrives, instead of the old
    /// optimistic clear-on-send that a rejection could never reopen.
    UiResponseAccepted(UiResponseAcceptedPayload),
    /// A field of a pending prompt changed on some surface: the station's
    /// shared draft, which every surface shows as the field's value. Last
    /// write wins. Nothing is submitted: the prompt resolves only on a
    /// `UiResponse`. The latest per field is replayed to a page that opens
    /// while the prompt is pending.
    UiDraftUpdated(UiDraftPayload),

    /// The CLI received a command that carried a `command_id`. Receipt,
    /// not execution: it tells the sender the station is listening, which
    /// a successful broker publish cannot.
    CommandReceived { command_id: String },

    /// The CLI stored a fresh session snapshot on the server (see
    /// `SessionSnapshot`). Dashboards waiting on a resync refetch on it.
    SnapshotStored { session_id: String, seq: u32 },

    // -- Identify-unit lifecycle --
    //
    // Identity is run metadata, not a phase. These three events form
    // the single contract for "the unit is being / has been
    // identified", regardless of source:
    //
    //   * `IdentifyRequest`  — pre-run operator prompt opens. Only
    //                           emitted when an operator scan is
    //                           required (no `auto_identify`). UIs
    //                           render the form on receipt.
    //   * `IdentifyResolved` — the unit (or any subset of its fields)
    //                           became known. Fires from every source:
    //                           pre-run prompt response, `auto_identify`
    //                           default resolution (no preceding
    //                           `IdentifyRequest`), mid-run operator
    //                           prompt response, and mid-run Python
    //                           bound-measurement updates. Field-level
    //                           merge — non-null fields overwrite, sub-
    //                           units merge by key.
    //   * `IdentifyTimeout`  — the pre-run prompt expired before the
    //                           operator answered; the CLI cancels the
    //                           run.
    /// Pre-run identify-unit operator prompt. Receipt of this event is
    /// the unambiguous signal "the operator must scan the next unit"
    /// — there is no shape heuristic on components and no synthetic
    /// phase wrapping.
    IdentifyRequest {
        request_id: String,
        /// Procedure the engine is identifying for. Carried so a UI
        /// hydrating mid-identify (page refresh, late attach) can show
        /// the procedure name in its topbar without waiting for
        /// `RunStarted`. Optional for back-compat with older CLIs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        components: Option<Vec<UiComponent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Identity of the run that will start once the operator
        /// answers. Pre-allocated by the CLI at `run::start()` so the
        /// UI can correlate this prompt with the upcoming `RunStarted`.
        /// Reducer drops a stale `IdentifyResolved` from a cancelled
        /// prior run that would otherwise poison `pendingUnitRef`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Unit identity (or a subset of its fields) became known. Fires
    /// pre-run AND mid-run; the UI does a field-level merge so a
    /// mid-run scan that only fills `sub_unit:wifi:serial_number`
    /// doesn't clobber the pre-run-set `serial_number`.
    IdentifyResolved {
        #[serde(skip_serializing_if = "Option::is_none")]
        slot_id: Option<String>,
        unit: UnitInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Who operates the run (the identify form's `operated_by`, the
        /// procedure's default). A run property, not the unit's: kept off
        /// `UnitInfo`. A phase may still change it (`run.operated_by`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operated_by: Option<String>,
        /// Per-run identity. Reducer drops a stale resolution from a
        /// cancelled prior run that would otherwise seed
        /// `pendingUnitRef` with the wrong unit between New Run click
        /// and the upcoming `RunStarted` — uploading the new run with
        /// a foreign serial number.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Pre-run identify prompt timed out.
    IdentifyTimeout {
        request_id: String,
        /// Per-run identity. Reducer drops cross-run leaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },

    // -- Config --
    /// Station config applied (or failed to apply)
    ConfigApplied {
        installation_id: String,
        key: String,
        value: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// The station re-read its config and secrets after a
    /// `StationCommand::VariablesChanged` (or on its own: a pull, a run
    /// start). Names and versions only travel through the server's
    /// delivery log; this ack carries none.
    VariablesApplied {
        installation_id: String,
        /// The resolve response the station's cache now holds (the
        /// server's `etag`). None when it has never fetched one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
        /// Why the refresh failed (server unreachable, refused). The
        /// station keeps running on its last cache, marked stale.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // -- CLI self-update --
    /// CLI update started (downloading or applying)
    UpdateStarted {
        installation_id: String,
        from_version: String,
        to_version: String,
    },
    /// CLI update applied; published by the new process after re-exec
    UpdateApplied {
        installation_id: String,
        from_version: String,
        to_version: String,
    },
    /// CLI update failed
    UpdateFailed {
        installation_id: String,
        from_version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_version: Option<String>,
        error: String,
    },

    /// Server-published acknowledgement that an installation was logged
    /// out. Mirrors the `StationCommand::Logout` (CLI-bound) but on the
    /// status channel so dashboard tabs can refresh activity / setup
    /// state without polling. Reason mirrors `LogoutReason` literals.
    LoggedOut {
        installation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Collaborative presence: where one participant is on the operator
    /// screen (focused field, live draft, pointer). Never on the run
    /// stream: it travels on its own `station_presence:<id>` channel,
    /// which keeps no history, so a moving pointer cannot evict run
    /// events. On that channel each client publishes a bare
    /// `PresenceState`; receivers build this event from it plus the
    /// broker-attached identity of the publishing connection, which a
    /// client cannot forge. The kiosk and TUI receive it from the CLI in
    /// the same shape.
    Presence(PresenceEvent),
}

/// Which realtime channel an event belongs on. One table, so the CLI's
/// publisher and every reader agree on where to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventChannel {
    /// `station_run:<id>`, numbered in a `RunEnvelope`: what the operator
    /// UI folds into its run state, and what a snapshot replays.
    Run,
    /// `station_health:<id>`: station-level state the server persists
    /// (telemetry, hardware, deployments, config, updates, sign-out).
    Health,
    /// `station_presence:<id>`: ephemeral, no history.
    Presence,
}

impl StationEvent {
    pub fn channel(&self) -> EventChannel {
        use StationEvent::*;
        match self {
            Hardware { .. }
            | Telemetry { .. }
            | VenvBootstrap { .. }
            | DeploymentPullStarted { .. }
            | DeploymentPullCompleted { .. }
            | DeploymentPullFailed { .. }
            | DeploymentRemoved { .. }
            | DeploymentAdded { .. }
            | ConfigApplied { .. }
            | VariablesApplied { .. }
            | UpdateStarted { .. }
            | UpdateApplied { .. }
            | UpdateFailed { .. }
            | LoggedOut { .. } => EventChannel::Health,
            Presence(_) => EventChannel::Presence,
            RunStarted { .. }
            | PhaseStarted { .. }
            | PhaseLog { .. }
            | MeasurementUpdate { .. }
            | AttachmentAdded { .. }
            | PhaseComplete { .. }
            | SequenceComplete { .. }
            | RunComplete { .. }
            | RunUploaded { .. }
            | RunUploadQueued { .. }
            | RunUploadStarted { .. }
            | RunUploadSucceeded { .. }
            | RunUploadFailed { .. }
            | RunUploadDropped { .. }
            | AttachmentUploaded { .. }
            | RunCrashed { .. }
            | RunProgress { .. }
            | PlugStatus { .. }
            | PlugLog { .. }
            | UiRequest { .. }
            | UiUpdate { .. }
            | UiResponseRejected { .. }
            | UiResponseAccepted { .. }
            | UiDraftUpdated { .. }
            | CommandReceived { .. }
            | SnapshotStored { .. }
            | IdentifyRequest { .. }
            | IdentifyResolved { .. }
            | IdentifyTimeout { .. } => EventChannel::Run,
        }
    }
}

fn one() -> u32 {
    1
}

/// Commands published by dashboard to the commands channel (Dashboard -> CLI).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StationCommand {
    Run {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_id: Option<String>,
        /// Pre-resolved unit data for the next run. When set, the CLI
        /// skips the identify-unit prompt entirely and jumps straight
        /// to phase execution with this unit. Used by the operator
        /// UI's "Run again" button on the outcome screen — same unit
        /// as the run that just finished, no re-scan needed. None on
        /// "New run" (and on any first-run command), which triggers
        /// the normal identify flow.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reuse_unit: Option<UnitInfo>,
        /// Multi-slot "Run again": the unit to reuse per slot key. A slot
        /// absent from the map is identified as usual, so a fixture can
        /// rerun with some nests reloaded. Takes precedence over
        /// `reuse_unit` for the slots it names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reuse_units: Option<std::collections::HashMap<String, UnitInfo>>,
        /// Email of the dashboard user who pressed Run from the web
        /// operator UI. Forwarded to `runs.create` v2 as `operated_by`
        /// so the resulting run is attributed to that user. None when
        /// the run is triggered from a kiosk-mode operator UI (no
        /// browser session) or from the CLI directly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operated_by: Option<String>,
        /// Partial run: execute only this main phase — plus its
        /// transitive `depends_on` closure and every setup/teardown
        /// phase. Sent by the Studio Builder's per-phase play button.
        /// None runs the whole procedure. Older daemons ignore the
        /// field, which is why the web gates the button on the
        /// `partial_run` hello capability.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_phase: Option<String>,
    },
    /// Upload a finished Studio run to a dashboard procedure the user
    /// picked. Studio runs execute under a local marker id and never
    /// auto-upload; the daemon retains the most recent finished run and
    /// this command publishes it. `execution_id` identifies that run;
    /// `procedure_id` is the destination procedure's UUID chosen in the
    /// web UI (there is no way to derive it from a local folder).
    /// Progress comes back as the standard `RunUpload*` events.
    UploadRun {
        execution_id: String,
        procedure_id: String,
    },
    UiResponse(UiResponseCommand),
    /// A surface edited a field of a pending prompt (`UiDraftUpdated`).
    UiDraft(UiDraftCommand),
    /// The operator pressed a panel button (`UiPanel::events`).
    UiEvent(UiEventCommand),
    /// Upload a fresh `SessionSnapshot` now. Sent by a dashboard whose
    /// live stream skipped a `seq` it cannot fill from channel history.
    RequestSnapshot {},
    ConfigUpdate {
        key: String,
        value: String,
    },
    /// The config or secrets this station resolves may have changed: the
    /// station fetches them again (`POST /api/cli/variables/resolve`) and
    /// answers `StationEvent::VariablesApplied`. Carries no value.
    VariablesChanged {},
    /// Check the deployments now. `forced` (the dashboard's "Pull now")
    /// also retries a deployment whose install failed, without waiting for
    /// the station's back-off. Absent on every automatic pull; a CLI that
    /// predates the field ignores it and runs a normal pull.
    Pull {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forced: Option<bool>,
    },
    /// Stop the in-flight run. The engine cancels the current
    /// phase, runs teardown, and emits a `RunComplete` with outcome
    /// `"ABORTED"`. UIs render the abort screen on receipt.
    Stop {
        /// Optional operator-provided reason for the abort, surfaced
        /// in the run timeline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The run it is meant for. A station ignores it when another run
        /// is active: a stop delivered late must not end the next unit's.
        /// Absent from older senders, which target whatever runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Force-kill the in-flight run. Skips teardown, SIGKILLs all
    /// workers and plug services in parallel, emits `RunComplete`
    /// with outcome `"ABORTED"`. Used as the escalation step when a
    /// `Stop` is hung in teardown. Sending `Kill` after `Stop` is
    /// idempotent — graceful shutdown is abandoned mid-flight.
    Kill {
        /// Optional operator-provided reason for the force-kill.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The run it is meant for (see `Stop`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// Skip the currently-running phase and continue with the next.
    /// Phase ends with outcome `"SKIP"`. No-op if no phase is
    /// running.
    SkipPhase {
        phase_key: String,
    },
    /// Force a retry of a phase that has terminated. Phase status
    /// flips back to running and a new attempt starts. Bounded by
    /// the procedure's retry limit; otherwise rejected by the
    /// engine with no event change.
    RetryPhase {
        phase_key: String,
    },
    /// Server-initiated logout: the CLI should clear credentials and exit.
    /// Used when a new installation replaces this one.
    Logout {
        /// Display-only reason, e.g. "replaced", "user". Optional so future
        /// server paths can publish a Logout without inventing a string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Optional installation id the command is intended for; CLIs ignore
        /// the command if it doesn't match their own installation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installation_id: Option<String>,
    },
    /// Force-retry an upload that's parked in the queue (4xx errors
    /// don't auto-retry; transient errors with a future
    /// `next_retry_at` skip ahead). No-op if the queue id is unknown.
    QueueRetry {
        queue_id: String,
    },
    /// Drop a queued upload outright. Attachments are deleted from
    /// disk; the queue entry is gone.
    QueueDrop {
        queue_id: String,
    },
    /// Operator-initiated graceful shutdown of the CLI process.
    /// Cancels any active run, asks the OS supervisor (systemd /
    /// launchd) to stop the unit without disabling launch_on_boot,
    /// then exits. Next reboot brings the station back. No-op
    /// (clean process exit) when running outside a supervisor.
    Exit {},
}

/// One required config value or secret without a value (`RunCrashed.missing`).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct MissingValue {
    /// `config` or `secret`.
    pub kind: String,
    pub key: String,
}

/// `StationCommand::UiResponse`: an operator's answer to a prompt.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiResponseCommand {
    pub request_id: String,
    pub values: std::collections::HashMap<String, String>,
    /// Who answered. Filled in by the CLI from the command envelope
    /// (dashboard) or the local surface (kiosk, TUI); never read from the
    /// wire, so a browser cannot answer in someone else's name.
    #[serde(skip)]
    #[cfg_attr(feature = "specta", specta(skip))]
    pub actor: Option<Actor>,
}

/// `StationCommand::UiDraft`: a field's new value, as a surface edits it.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiDraftCommand {
    pub request_id: String,
    pub component_key: String,
    /// Wire encoding of the value, as in `UiResponse` values.
    pub value: String,
    /// The editing surface's own id (one per page or TUI), echoed on the
    /// event so it can tell its own edit coming back.
    pub origin: String,
    /// Increasing per `origin`. The station drops a draft of a field
    /// older than one it already took from the same origin: a dashboard's
    /// drafts travel as separate requests and may land out of order.
    pub seq: u32,
    /// Who edited. Filled in like `UiResponseCommand::actor`.
    #[serde(skip)]
    #[cfg_attr(feature = "specta", specta(skip))]
    pub actor: Option<Actor>,
}

/// `StationCommand::UiEvent`: a press of the panel button `name` on the
/// screen `request_id`. Counted for the phase (`ui.pressed(name)`), never
/// recorded; dropped when that screen is gone or the name is not one of
/// its panel's `events`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiEventCommand {
    pub request_id: String,
    pub name: String,
    /// Who pressed. Filled in like `UiResponseCommand::actor`.
    #[serde(skip)]
    #[cfg_attr(feature = "specta", specta(skip))]
    pub actor: Option<Actor>,
}

/// `StationEvent::UiDraftUpdated`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiDraftPayload {
    pub request_id: String,
    pub component_key: String,
    pub value: String,
    /// The sender's `origin` and `seq`, echoed: a surface skips its own
    /// edit coming back only while it holds a newer one.
    pub origin: String,
    pub seq: u32,
    /// Who edited, as the station stamped them.
    pub by: Actor,
}

/// `StationEvent::UiResponseAccepted`. A struct so the optional fields
/// stay optional in the generated TypeScript (older CLIs omit them).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiResponseAcceptedPayload {
    pub request_id: String,
    /// Who resolved the prompt. On a lost race this is the winner, not
    /// the submitter: a surface whose own submission is answered with
    /// someone else's name knows its values were not used. None from a
    /// CLI that predates it, or when nobody's answer is on record (the
    /// prompt timed out, the run moved on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Actor>,
}

impl UiResponseAcceptedPayload {
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            by: None,
        }
    }
}

/// Who caused something on the station: a command, a prompt answer.
/// Stamped by whoever authenticated the sender (the dashboard API from
/// the session, the CLI for its own kiosk and TUI), never by the sender.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Actor {
    pub kind: ActorKind,
    /// Dashboard user id. None for the station's own operator and for
    /// server automation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A signed-in dashboard user, acting remotely.
    User,
    /// Someone at the station: kiosk or TUI.
    Station,
    /// The server acting on its own (a replaced installation, a config push).
    System,
}

impl Actor {
    /// The person at the station, as the CLI stamps its own surfaces.
    pub fn station() -> Self {
        Actor {
            kind: ActorKind::Station,
            user_id: None,
            display_name: "Station operator".into(),
        }
    }
}

/// What arrives on `station_cmd:<id>`: a `StationCommand` with the
/// server's stamp flattened beside its `type`, so a CLI that predates the
/// stamp still parses the command and ignores the extra keys. The kiosk
/// sends bare commands, which parse with both fields empty.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommandEnvelope {
    /// Set by the dashboard API; the CLI answers `CommandReceived` with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    /// When the server published it, ms since the epoch. The CLI drops a
    /// command older than its staleness budget: the command channel keeps
    /// a short history so a reconnecting CLI still gets what it missed,
    /// and this is what stops a `kill` from that history landing on the
    /// next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at_ms: Option<u64>,
    #[serde(flatten)]
    pub command: StationCommand,
}

/// Version of the station ↔ dashboard realtime wire format: every type a
/// station and a dashboard exchange through Centrifugo, the channels they
/// use and what `/api/cli/stream` returns. Negotiated once per connection:
/// the CLI sends it (`X-Realtime-Protocol`), the dashboard accepts it when
/// it lies in its supported range and otherwise refuses with the side that
/// must upgrade.
///
/// Within one version every change is additive: a new event, command or
/// optional field, which an older receiver ignores. Anything an older
/// peer would misread (a removed or renamed field, a changed type, a new
/// required field, a moved channel) bumps it. `tests/wire_compat.rs`
/// enforces that against frozen fixtures; see the crate README.
///
/// 1 was the single `station:<id>` namespace (CLI ≤ 2.0); 2 is the
/// per-traffic namespaces with numbered run events (CLI 2.1).
pub const REALTIME_PROTOCOL: u32 = 2;

/// How old a command may be when the CLI receives it. Above the command
/// channel's history TTL, so a replay within the window is applied once
/// (deduplicated by `command_id`) and anything older is dropped.
pub const COMMAND_MAX_AGE_MS: u64 = 45_000;

impl CommandEnvelope {
    /// The command, with the envelope's actor moved onto the variants
    /// that carry one internally (`UiResponse`).
    pub fn into_command(self) -> StationCommand {
        match self.command {
            StationCommand::UiResponse(mut answer) => {
                answer.actor = self.actor;
                StationCommand::UiResponse(answer)
            }
            StationCommand::UiDraft(mut draft) => {
                draft.actor = self.actor;
                StationCommand::UiDraft(draft)
            }
            StationCommand::UiEvent(mut press) => {
                press.actor = self.actor;
                StationCommand::UiEvent(press)
            }
            other => other,
        }
    }
}

/// What the CLI publishes on `station_run:<id>`: every run-scoped event,
/// numbered once at the source. `seq` is gapless within a `session_id`
/// (one per CLI process, across its reconnects), so a receiver that sees
/// a jump knows it lost events, including the ones the CLI itself dropped
/// before they reached the broker (a lagged bus, an event too large to
/// publish, a link that was down). A new `session_id` means the CLI
/// restarted: resync, never merge.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunEnvelope {
    pub session_id: String,
    /// u32 because the browser reads it as a JS number. A publisher that
    /// would wrap starts a new `session_id` instead.
    pub seq: u32,
    pub event: StationEvent,
}

/// Consecutive run events in one publication: `events[i]` is numbered
/// `seq + i` in `session_id`. A burst (a run that ends at once, a Stop
/// skipping every remaining phase) costs the broker's publish proxy one
/// call instead of one per event. Published only to a server that
/// advertised `run_batch` in its `/api/cli/stream` answer; receivers
/// unpack it into `RunEnvelope`s and treat each as if it came alone.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunBatch {
    pub session_id: String,
    pub seq: u32,
    pub events: Vec<StationEvent>,
}

/// Most events one `RunBatch` carries. A burst past it is several
/// batches; a receiver refuses a longer one.
pub const RUN_BATCH_MAX_EVENTS: usize = 64;

impl RunBatch {
    /// The envelopes it stands for, in order.
    pub fn envelopes(self) -> impl Iterator<Item = RunEnvelope> {
        let session_id = self.session_id;
        let first = self.seq;
        self.events
            .into_iter()
            .enumerate()
            .map(move |(i, event)| RunEnvelope {
                session_id: session_id.clone(),
                seq: first.saturating_add(i as u32),
                event,
            })
    }
}

/// The CLI's replay of its current run, stored on the server so a
/// dashboard that opens mid-run starts from the station's own state
/// instead of whatever channel history still holds. Same content the
/// kiosk gets on hydrate: the pinned `run_started`, open prompts, then
/// the bounded tail, in order. `seq` is the last envelope it covers.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SessionSnapshot {
    pub session_id: String,
    pub seq: u32,
    pub events: Vec<StationEvent>,
    /// Some of the run's events never made it into the replay (the CLI's
    /// event bus lagged, or the snapshot was cut to its size budget): a
    /// dashboard shows what it has and says the run is not all there.
    // `default` last, so the TS export reads `partial?: boolean`.
    #[serde(skip_serializing_if = "std::ops::Not::not", default = "default_false")]
    pub partial: bool,
}

/// Presence values every receiver (CLI, dashboard) applies alike to the
/// identity signed into a connection: exported to TypeScript so the two
/// never drift.
pub mod presence_limits {
    /// The station's own operator (kiosk tabs, TUI).
    pub const STATION_COLOR: &str = "#14B8A6";
    /// For a connection whose identity says no name or color.
    pub const FALLBACK_NAME: &str = "Someone";
    pub const FALLBACK_COLOR: &str = "#64748B";
    /// Longest avatar URL loaded; it rides every presence message.
    pub const MAX_AVATAR_URL_LEN: usize = 2048;
    /// A role is a lowercase word of at most this many letters.
    pub const MAX_ROLE_LEN: usize = 16;
    /// Participants one connection may show as: a station's kiosk tabs and
    /// TUI share its one connection, more than this is made up.
    pub const SOURCES_PER_CONNECTION: usize = 8;
}

/// What a local operator-UI server advertises on its hello, read by the
/// kiosk and Studio to gate features on the CLI they face.
pub mod capabilities {
    /// Every `ui_response` is answered with a verdict event.
    pub const UI_RESPONSE_VERDICT: &str = "ui_response_verdict";
    /// The station daemon's runs take `ui_draft` (shared prompt editing).
    pub const SHARED_DRAFTS: &str = "shared_drafts";
}

/// One participant's presence, as receivers see it.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresenceEvent {
    pub participant: PresenceParticipant,
    /// None: the participant left (tab closed, connection dropped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PresenceState>,
}

/// Identity of a presence publisher, taken from its authenticated
/// connection (Centrifugo `info`), not from what it published.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresenceParticipant {
    /// Unique per connection (and per local surface behind the station's
    /// one connection), so the same user in two tabs shows twice.
    pub id: String,
    pub kind: ActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub display_name: String,
    /// Hex color (e.g. `"#EA580C"`), assigned by the server per user.
    pub color: String,
    /// The user's profile picture, signed into the connection like the
    /// rest. None for the station's own operator and users without one:
    /// receivers show initials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// The user's role in the organization (`owner`, `admin`,
    /// `developer`, `operator`, `viewer`), signed like the rest: tells the
    /// others why a viewer's cursor never edits. None for the station's
    /// own operator. Receivers show only the roles they know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// What a participant publishes on the presence channel. Every field is
/// the full current value: a receiver replaces, never merges.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresenceState {
    /// Monotonic per publisher. Receivers drop anything not greater than
    /// the last seen, so a reordered delivery never moves a cursor back.
    pub seq: u32,
    /// Which prompt field the participant is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<PresenceFocus>,
    /// Live text in that field. Soft lock: others should not type into
    /// it while the draft lasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<PresenceDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<PresencePointer>,
    /// Which local surface published, for the station's kiosk tabs and
    /// TUI, which share the CLI's one connection. Dashboards leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// On the operator screen, whether or not doing anything there: what
    /// the "who is here" avatars show. Heartbeated like the rest; a state
    /// with nothing set, `here` included, is the participant leaving.
    // `default` last, so the TS export reads `here?: boolean` (see
    // `locked` below).
    #[serde(skip_serializing_if = "std::ops::Not::not", default = "default_false")]
    pub here: bool,
}

/// A pointer position, relative to an element both sides render, because
/// the kiosk and the dashboard lay the same screen out at different sizes.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresencePointer {
    /// The anchor the coordinates are relative to: a prompt component, or
    /// the operator screen's centered column when None. A receiver that
    /// does not render the anchor hides the pointer rather than guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<PresenceFocus>,
    /// 0..1 across the anchor's box. Without an anchor: from the column's
    /// left edge in column widths, outside 0..1 beside the column.
    pub x: f32,
    /// 0..1 down the anchor's box. Without an anchor: from the column's
    /// top in column widths (its height depends on the screen, its width
    /// does not).
    pub y: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresenceFocus {
    pub request_id: String,
    pub component_key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PresenceDraft {
    pub request_id: String,
    pub component_key: String,
    pub value: String,
    /// Caret position within `value` (UTF-16 code units to match
    /// browser text inputs). Optional for publishers that don't track
    /// caret — e.g. a future TUI without selection-aware input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_pos: Option<u32>,
}

// ---------------------------------------------------------------------------
// Studio RPC (dashboard Studio <-> CLI daemon)
// ---------------------------------------------------------------------------
//
// Request/response pairs for the Studio surface: file browsing, file
// read/write, and procedure validation. These do NOT ride the event
// broadcast channels — payload-bearing replies go over request/response
// transports only (loopback HTTP today, cloud-forwarded RPC later).
// Keeping them in this crate gives Studio the same Rust→TS codegen and
// CI drift check as the station wire types.
//
// Compatibility policy: additive-only. New ops/response variants may be
// added; existing fields are never repurposed. Servers answer an
// unrecognized op with `Error { code: Unsupported }` rather than
// dropping the request, so a newer dashboard talking to an older daemon
// gets a typed failure instead of a hung promise.

/// A Studio operation requested by the dashboard.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StudioRequest {
    /// Project metadata for the daemon's studio root.
    ProjectInfo {},
    /// List entries of a directory (relative to the studio root).
    /// `dir: None` lists the root itself.
    ListFiles {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
    },
    /// Read a text file (relative to the studio root).
    ReadFile { path: String },
    /// Write a text file. `expected_sha256` enables optimistic
    /// concurrency: when set and the on-disk content no longer hashes
    /// to it, the daemon refuses with `Error { code: Conflict }`.
    WriteFile {
        path: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
    },
    /// Create a directory (and any missing parents) under the studio
    /// root. `WriteFile` already creates the parents a file needs, so
    /// this covers the one thing it cannot express: an empty directory,
    /// made before there is a file to put in it.
    CreateDir { path: String },
    /// Validate the project's procedure definition. `path: None`
    /// validates the root's default procedure file.
    Validate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Validate proposed procedure content WITHOUT writing it to disk.
    /// `path` is where the file would live (it carries the extension
    /// check and, later, relative resolution); the target need not
    /// exist. Lets a client surface diagnostics on a proposal before
    /// asking the user to approve the write. Replies `Diagnostics`.
    ValidateContent { path: String, content: String },
    /// Write a binary project resource from a base64 payload. Confined
    /// to the `resources/` subtree, extension-allowlisted and
    /// size-capped by the daemon; resource bytes never transit through
    /// a model. Unlike `write_file` there is no diff to review, so the
    /// guard is coarser: an existing target is refused with
    /// `Error { code: Conflict }` (message carries its sha256) unless
    /// `overwrite` is set — what sits in `resources/` is what a running
    /// procedure reads off the bench, and must not vanish on a filename
    /// collision. Replies `Written` with the sha256 of the decoded bytes.
    WriteResource {
        path: String,
        content_base64: String,
        #[serde(default)]
        overwrite: bool,
    },
    /// Structured view of the project's procedure, parsed by the
    /// engine's real loader. This is the single source the sequence
    /// tree / inspector consume — clients never parse YAML themselves.
    GetSequence {},
    /// The config and secrets the active procedure (or the one at `path`)
    /// declares, and whether a source run would find a value for each in
    /// the shell or the procedure's `.env`. Key names and status only:
    /// no value, and nothing of the `.env` content, ever leaves the
    /// daemon. Answers `EnvStatus`.
    EnvStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// The Python environment the active procedure's code runs in:
    /// interpreter, version, venv, where its dependencies are declared
    /// and what is installed. Resolved through the SAME `ensure_venv`
    /// discipline as Run and the plug debugger, so the answer can never
    /// name an environment the procedure will not actually use — and,
    /// like them, it PROVISIONS a missing venv rather than reporting
    /// one. Refused `Busy` while a run is active, and under an open plug
    /// debug session when the venv would be built or rebuilt (the
    /// session's process runs on it): provisioning can
    /// rebuild the venv the run is executing in. Answers `PythonEnv`.
    PythonEnv {},
    /// The procedure YAML's JSON Schema as THIS engine accepts it: every
    /// field the loader knows, with its description. The assistant takes
    /// its field inventory from here, so the list matches the engine that
    /// will run the file rather than the web build. Static per daemon
    /// build; a client may cache it by the `version` the answer carries.
    /// Answers `ProcedureSchema`.
    ProcedureSchema {},
    /// Projects this session may switch between: the roots a human
    /// granted, past sessions included. Never a filesystem listing —
    /// the dashboard cannot enumerate the host from here.
    ListProjects {},
    /// Make one of the granted roots active. `path` must already be in
    /// the granted set: this op selects, it never grants. An unknown
    /// path is `Forbidden`, whether or not it exists on disk, so the
    /// reply cannot be used to probe the filesystem.
    OpenProject { path: String },
    /// Move a file or directory to the OS trash. The FIRST destructive
    /// op on this surface — everything before it only read or added.
    ///
    /// Trash rather than unlink, deliberately: the caller is a human in
    /// a file tree, and a mis-click on a procedure has to be
    /// recoverable outside Studio. A directory goes with its contents,
    /// which is what the trash gesture means everywhere else.
    ///
    /// There is no `expected_sha256` equivalent here (a hash cannot
    /// describe a tree), so the UI owes the user a confirmation naming
    /// what goes — the daemon cannot make that judgement.
    DeleteEntry { path: String },
    /// Copy `from` to `to`, both root-relative. A directory is copied
    /// with its contents.
    ///
    /// A server op rather than read+write from the page, for two
    /// reasons: `ReadFile` refuses anything outside the text allow-list,
    /// so a client-side copy could not carry a `.png`, and a directory
    /// would need one round trip per entry.
    ///
    /// Two things are deliberately NOT copied, matching what the tree
    /// shows rather than what the disk holds: symlinks (following one
    /// would pull in data from outside the project) and the excluded
    /// directories (`.git`, `venv`, `node_modules`, …) — duplicating a
    /// procedure folder should not duplicate its virtualenv.
    CopyEntry { from: String, to: String },
    /// Move `from` to `to`, both root-relative. One op covers rename
    /// (same parent, new name) and move (new parent): they are the same
    /// filesystem operation, and splitting them would duplicate the
    /// confinement checks that are the whole risk here.
    ///
    /// Refuses when `to` already exists rather than overwriting —
    /// clobbering someone's file is not a rename.
    MoveEntry { from: String, to: String },
    /// Make one of the project's discovered procedures active. `path`
    /// must be one of the paths `ProjectInfo` listed: like
    /// `OpenProject`, this selects among what the daemon found, it
    /// never takes an arbitrary path from the browser. Refused `Busy`
    /// during a run — the run pinned the procedure it started.
    OpenProcedure { path: String },
    /// Ask the daemon to open the OS folder picker on its own host —
    /// the ONE way a session gains a root it does not already hold.
    /// The dialog is native and modal: the page can request it but can
    /// neither drive it nor read anything from it except the final
    /// choice, so it is the human at the machine who grants. A chosen
    /// folder opens whatever it holds: picking it in the dialog is as
    /// explicit as typing its path, and an IDE does not ask "are you
    /// sure?" about the folder it was just told to open. Only the
    /// never-a-project roots (`/`, the ancestors of the home directory)
    /// are refused, `Forbidden`. Answers `Opened` with the picked
    /// project, `Error { code: Cancelled }` when the human dismissed
    /// the dialog, and `Busy` while a run is in flight or another
    /// picker window is already open. The one request, with
    /// `ListProjects` and `ProjectInfo`, that a session with NO project
    /// open accepts — it is how such a session gets one.
    PickProject {},
    /// List the methods of a plug's Python class, WITHOUT instantiating
    /// it — a one-shot introspection subprocess imports the module and
    /// reads the class object, so browsing a plug never touches
    /// hardware. `plug_key` must name a plug of the active procedure;
    /// the daemon resolves the file/class from the YAML itself and
    /// never takes a path from the browser. Refused `Busy` while a run
    /// is active: the import resolves the venv the same way Run does,
    /// which can rebuild it under the run. Answers `PlugMethodList`.
    PlugMethods { plug_key: String },
    /// Start a debug session: spawn the plug's service process outside
    /// any run. Refused `Busy` while a run is active — a run's plugs
    /// are its own — and under an open plug debug session when the
    /// venv would be built or rebuilt. `config_overrides_json`,
    /// when present, is a JSON object merged over the YAML `config`
    /// map for THIS session only (the file is never written); it is an
    /// opaque string because specta cannot export `serde_json::Value`.
    /// Answers `PlugDebugStarted`; log lines stream as `plug_log`
    /// events with no `execution_id`.
    PlugDebugStart {
        plug_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_overrides_json: Option<String>,
    },
    /// End a debug session: Cleanup + Shutdown of the plug's service
    /// process. Stopping a plug with no session is a no-op answered
    /// `PlugDebugStopped`, not an error — teardown must be idempotent
    /// so every owner (mode exit, run start, tab close) can fire it.
    PlugDebugStop { plug_key: String },
    /// Call one method on a debug-session plug. `args_json` is a JSON
    /// array of positional arguments, `kwargs_json` a JSON object of
    /// keyword arguments — both optional. Answers `PlugDebugResult`
    /// whether the Python call succeeded or raised; transport-level
    /// failures (no session, service dead) answer `Error`, and a stop
    /// that landed while the call waited or ran answers `Cancelled`.
    PlugDebugCall {
        plug_key: String,
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kwargs_json: Option<String>,
    },
    /// List the plug keys with a live debug session. The page calls it
    /// on mount to resync after a reload — sessions live in the daemon
    /// and survive the browser.
    PlugDebugSessions {},
    /// Classify a field regex and optionally diagnose a value against
    /// it, using the daemon's `pattern_messages` module — the single
    /// source of operator-facing pattern wording (TP-760). The web
    /// Studio editors call this instead of re-implementing any of it
    /// client-side: the sequence editor for its authoring nudge and
    /// for checking author-typed unit values.
    ValidatePattern {
        /// Regex to check; `None` means the server's unit-identity
        /// charset (`UNIT_FIELD_CHARSET_PATTERN`), so the client never
        /// hardcodes it.
        pattern: Option<String>,
        /// Value to diagnose; `None` / empty classifies only.
        value: Option<String>,
    },
    /// Judge a whole prompt submission against its components with
    /// `validate::validate_response` — the same call the `ui_response`
    /// chokepoint makes before it resolves a waiting phase. The web
    /// Studio's screen preview submits through this so the preview
    /// rejects exactly what a run would reject; the daemon touches no
    /// run state answering it, and a preview needs no run.
    ValidateResponse {
        /// The prompt's components, as the previewed screen renders
        /// them.
        components: Vec<UiComponent>,
        /// Component key -> submitted wire string, the shape
        /// `StationCommand::UiResponse` carries.
        values: std::collections::HashMap<String, String>,
    },
    /// Write (or overwrite) the `tofupilot.json` link at a discovered
    /// procedure's directory — the same file `tofupilot link` writes
    /// and `tofupilot run --upload` reads, so a destination chosen in
    /// Studio also serves CLI runs. `path` must name a procedure the
    /// daemon discovered (same membership rule as `open_procedure`),
    /// so this cannot write outside a procedure directory. Answers
    /// `ProcedureLinkSet`. Capability: `link_procedure`.
    LinkProcedure {
        path: String,
        procedure_id: String,
        /// Dashboard display name, persisted so CLI output can print
        /// something readable without an API round-trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_name: Option<String>,
    },
    /// Delete the `tofupilot.json` link at a discovered procedure's
    /// directory. Removing an absent link is a no-op answered
    /// `ProcedureLinkSet { link: None }`, not an error — unlink must be
    /// idempotent. Answers `ProcedureLinkSet`.
    UnlinkProcedure { path: String },
}

/// A project root the session can switch to.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioProject {
    /// Absolute path on the daemon host. Display and identity only —
    /// file requests stay relative to whichever root is active.
    pub path: String,
    /// The directory's own name.
    pub name: String,
}

/// The dashboard procedure a local procedure directory is bound to,
/// read from its `tofupilot.json` (written by `tofupilot link` or the
/// `link_procedure` op). Wire mirror of the CLI's `ProcedureLink`,
/// snake_cased like the rest of this protocol.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioProcedureLink {
    pub procedure_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure_name: Option<String>,
}

/// A procedure the daemon found inside the active project. One project
/// can hold several: a repo with one procedure per subdirectory is the
/// shape the git integration already imports, so the local session has
/// to describe it too.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioProcedure {
    /// Root-relative path of the procedure definition, e.g.
    /// `plug-rpc-60s-timeout/procedure.yaml`. Identity of the
    /// procedure and what `OpenProcedure` takes.
    pub path: String,
    /// Root-relative directory holding it, `""` for the project root.
    /// This is the directory a run executes.
    pub dir: String,
    /// Display name: the definition's own `name:` when it reads, else
    /// the holding directory's name. Never a file name — the file name
    /// is canonical, the folder is what the author chose.
    pub name: String,
    /// Dashboard procedure this directory is linked to, when its
    /// `tofupilot.json` exists and parses. Absent otherwise — a
    /// malformed file reads as "not linked" here (re-linking rewrites
    /// it), unlike the CLI which warns; a per-poll daemon warning
    /// would only spam the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<StudioProcedureLink>,
}

/// One declared config value or secret in `StudioResponse::EnvStatus`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioEnvKey {
    /// `config` or `secret`.
    pub kind: String,
    pub key: String,
    pub required: bool,
    /// Where a source run takes its value from: `shell`, `dotenv` or
    /// `default`. None when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Required and without a value: a run is refused (not configured).
    pub missing: bool,
}

/// Reply to a `StudioRequest`. Every request maps to exactly one
/// variant (or `Error`).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum StudioResponse {
    /// `EnvStatus`: every declared key, config first, then by name.
    EnvStatus {
        /// Root-relative path of the procedure's `.env`, whether or not
        /// it exists.
        dotenv_path: String,
        dotenv_exists: bool,
        keys: Vec<StudioEnvKey>,
    },
    /// `ProjectInfo`'s answer while a project is open. See `NoProject`
    /// for the other answer the same request can get.
    ProjectInfo {
        /// Absolute studio root on the daemon host. Display-only for
        /// the dashboard; all request paths stay root-relative.
        root: String,
        name: String,
        /// The daemon's own semver (`CARGO_PKG_VERSION`), so the
        /// dashboard can warn below its manually-set recommendation
        /// (`RECOMMENDED_DAEMON_VERSION`, apps/web
        /// `studio/daemon-version.ts`) — a "please update, some
        /// features may not work" toast, never a refusal. Optional on
        /// the wire because released daemons predate the field —
        /// exactly the daemons worth warning about, which is why the
        /// dashboard treats "absent" as below any recommendation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        /// Root-relative path of the ACTIVE procedure definition (e.g.
        /// `procedure.yaml`), when the project holds one. Kept as the
        /// single active path it always was: older dashboards read only
        /// this and must keep working.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedure_path: Option<String>,
        /// Every procedure found under the root, the active one
        /// included, ordered root-first then by path. This daemon
        /// always sends `Some` (an empty list on a folder holding
        /// none), but the field is optional on the wire because the
        /// dashboard also meets OLDER daemons that predate it — the
        /// web already models that as `?? null`; the type now says so
        /// instead of contradicting it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        procedures: Option<Vec<StudioProcedure>>,
        /// The daemon host's OS as Rust names it (`macos`, `windows`,
        /// `linux`). Sent so a client — the assistant above all — stops
        /// guessing the shell, the path separator and the install
        /// command of the machine it is editing. Optional on the wire
        /// because released daemons predate it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        os: Option<String>,
        /// The host's CPU architecture (`aarch64`, `x86_64`). Same
        /// reason: which wheel exists is an os+arch question.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arch: Option<String>,
    },
    /// `ProjectInfo`'s answer when the session holds NO project: the
    /// daemon was launched with no argument and no previous project to
    /// reopen. The page shows its welcome state (open a folder, ask
    /// what Studio is) instead of an IDE. Every file, procedure and run
    /// request is refused with `Error { code: NoProject }` until a
    /// `PickProject` grants a root — there is nothing to confine them
    /// to. A distinct variant rather than an optional `root`: no
    /// consumer can mistake an empty string for a folder.
    NoProject {
        /// Same as `ProjectInfo::version`, for the same update toast.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Files {
        entries: Vec<StudioFileEntry>,
    },
    FileContent {
        path: String,
        content: String,
        sha256: String,
    },
    Written {
        path: String,
        sha256: String,
    },
    DirCreated {
        /// Root-relative path of the directory that now exists.
        path: String,
    },
    Deleted {
        /// Root-relative path that is gone from the project.
        path: String,
    },
    Moved {
        from: String,
        to: String,
    },
    Copied {
        from: String,
        to: String,
    },
    Diagnostics {
        diagnostics: Vec<StudioDiagnostic>,
    },
    Sequence {
        sequence: StudioSequence,
    },
    Projects {
        /// Granted roots, most recently opened first. Roots whose
        /// directory has disappeared are omitted: offering to reopen a
        /// path that is gone is worse than not offering it.
        projects: Vec<StudioProject>,
        /// Absolute path of the active root, `None` in a session with
        /// no project open. Usually one of `projects`, but NOT
        /// guaranteed: the vanished-directory filter above also applies
        /// to the active root (a disconnected share), so consumers must
        /// key on the path, not on finding it in the list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<String>,
    },
    Opened {
        project: StudioProject,
    },
    ProcedureOpened {
        procedure: StudioProcedure,
    },
    /// `LinkProcedure` / `UnlinkProcedure`'s answer: the procedure's
    /// link as it now stands on disk (`None` after an unlink).
    ProcedureLinkSet {
        /// Root-relative procedure path the link belongs to, echoed
        /// back. (Today's page refetches `project_info` rather than
        /// patching a row, so this is identification, not an update
        /// channel.)
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<StudioProcedureLink>,
    },
    /// `PythonEnv`'s answer: the environment the active procedure runs in.
    PythonEnv {
        /// Absolute interpreter path, e.g. `<project>/venv/bin/python`.
        python: String,
        /// Absolute venv directory holding that interpreter.
        venv: String,
        /// `X.Y.Z` as the interpreter itself reports it.
        python_version: String,
        /// Where this procedure's dependencies are declared. `None` is
        /// not an error — it means the venv was created empty, so every
        /// third-party import fails until a deps file exists.
        deps_source: StudioDepsSource,
        /// Display path of that file, absent on `"none"`. Root-relative
        /// when it sits inside the project (see `deps_in_project`),
        /// absolute when it is an ancestor uv-workspace pyproject.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deps_path: Option<String>,
        /// Whether `deps_path` is root-relative, i.e. reachable by the
        /// file ops of this protocol. False for a workspace pyproject
        /// above the project root: it governs the venv but cannot be
        /// edited from here.
        deps_in_project: bool,
        /// Installed distributions as `name==version`: the ones the
        /// dependency file declares first, then the rest, each part
        /// sorted case-insensitively; capped
        /// at 200. The cap is the daemon's (`PYTHON_ENV_PACKAGE_CAP` in
        /// `apps/cli/src/local_ws/studio.rs`), not a wire constant:
        /// specta exports types, not consts, so a number here would
        /// reach no client. The CI driver
        /// `apps/cli/tests/studio/python_env_rpc.py` restates it, and
        /// this comment is what keeps the two honest.
        packages: Vec<String>,
        /// Total installed distributions before the cap.
        package_count: u32,
    },
    /// `ProcedureSchema`'s answer: the schema document, serialized.
    ProcedureSchema {
        /// This daemon's version, the cache key for the schema.
        version: String,
        /// The JSON Schema (draft 2020-12) of `procedure.yaml`, as JSON
        /// text. Rendered into the assistant's prompt by the web app.
        schema_json: String,
    },
    /// `PlugMethods`' answer: the class's own methods, source order.
    PlugMethodList {
        plug_key: String,
        methods: Vec<StudioPlugMethod>,
    },
    PlugDebugStarted {
        plug_key: String,
    },
    PlugDebugStopped {
        plug_key: String,
    },
    /// `PlugDebugCall`'s answer. `success: false` with `error` set is a
    /// Python-level failure (an exception inside the method) — the call
    /// itself completed, which is why it is not a `StudioResponse::Error`.
    PlugDebugResult {
        plug_key: String,
        method: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_ms: u32,
    },
    PlugDebugSessionList {
        plug_keys: Vec<String>,
    },
    /// Reply to `ValidateResponse`: component key -> operator-facing
    /// message, empty when the submission would be accepted. Same map
    /// as `StationEvent::UiResponseRejected.errors`, produced by the
    /// same validator — a preview and a run cannot word a rejection
    /// differently.
    ResponseVerdict {
        errors: std::collections::HashMap<String, String>,
    },
    /// Reply to `ValidatePattern`.
    PatternVerdict {
        /// Whether the pattern parses as a regex at all.
        valid: bool,
        /// Whether operator messages derive from the pattern by itself
        /// — when false, the authoring nudge fires (a `pattern_message`
        /// is the only way the operator learns the expected format).
        derivable: bool,
        /// Whether `value` matches the pattern in full: `None` when no
        /// value was sent or the pattern does not compile. `message`
        /// alone cannot say — a structural pattern derives nothing, so
        /// a mismatch there reads exactly like a match.
        matches: Option<bool>,
        /// Derived message for `value`: `None` when the value matches,
        /// no value was sent, or nothing can be derived.
        message: Option<String>,
    },
    Error {
        code: StudioErrorCode,
        message: String,
    },
}

/// One method of a plug class, as reported by `PlugMethods`. Only
/// methods defined in the class's own body are listed (inherited
/// library members are noise on a bench); `__init__` is included —
/// its parameters are what the YAML `config` map feeds.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioPlugMethod {
    pub name: String,
    /// First line of the method's own docstring, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Line of the `def` in the source file — the list's sort key, so
    /// the UI reads like the file does.
    pub lineno: u32,
    pub params: Vec<StudioPlugParam>,
}

/// One parameter of a plug method, `self` excluded.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioPlugParam {
    pub name: String,
    /// Parameter kind AS THE CALL ACCEPTS IT, not as the `def` line
    /// declares it: `positional_or_keyword`, `keyword_only`,
    /// `positional_only`, `var_positional`, `var_keyword`. A phase calls
    /// a plug method through a proxy that passes a positional list (the
    /// phase call's convention, not a wire limit), so its
    /// `positional_or_keyword` parameters are reported `positional_only`
    /// and a `keyword_only` one cannot be passed at all (tp_worker.py,
    /// `PLUG_CALL_ACCEPTS_KEYWORDS`);
    /// `__init__` keeps its declared kinds, the runner constructs the
    /// class with the YAML config as keyword arguments.
    pub kind: String,
    /// True when the signature has no default — the UI must collect a
    /// value (or find one in the YAML config) before calling.
    pub required: bool,
    /// The signature default rendered as JSON, when representable
    /// (`None` for required params and non-JSON defaults).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_json: Option<String>,
    /// The annotation's display string (`float`, `str | None`, …),
    /// when annotated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
}

/// Structured procedure view for the Studio UI, derived from the
/// engine's parsed `ProcedureDefinition`. Deliberately a display
/// projection, not the full model: writes stay full-file text, so
/// this never needs to round-trip back to YAML.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequence {
    pub name: String,
    pub version: String,
    // Never skipped when empty, for the same reason as the lists below:
    // the generated TS declares it non-optional, and omitting it made
    // the Inspector's Description field crash on `undefined.trim`.
    // No `serde(default)` either: specta reads it as "optional" and
    // would emit `description?: string` — the lying type again. Nothing
    // deserializes this projection in Rust, only the CLI builds it.
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<StudioSequenceUnit>,
    /// Procedure-root `operated_by:` prompt config — run attribution
    /// (`runs.create` `operated_by`), a run property distinct from the
    /// unit fields even though it is prompted on the same
    /// identification screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<StudioSequenceUnitField>,
    // Lists are NEVER skipped when empty: the generated TS declares
    // them non-optional, and omitting them on the wire made the UI
    // crash on `undefined.map`. Bytes saved are not worth a lying type.
    pub plugs: Vec<StudioSequencePlug>,
    pub setup: Vec<StudioSequencePhase>,
    pub main: Vec<StudioSequencePhase>,
    pub teardown: Vec<StudioSequencePhase>,
    /// The sequence calls the procedure makes, parents before children,
    /// in file order. The phases they brought in are in the three lists
    /// above with a `sequence_path`; this is the tree over them. Absent
    /// from daemons that predate sequences, hence optional.
    // Always sent, empty included (see the lists above). `default` alone
    // is what types it optional: a daemon older than sequences omits it.
    #[serde(default)]
    pub sequences: Vec<StudioSequenceCall>,
}

/// One resolved `uses:` entry, for the Sequence tree and the call's
/// Inspector page.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceCall {
    /// Instance keys from the procedure root to this call, this call's
    /// key last; depth 1 is a call written in the procedure.
    pub path: Vec<String>,
    /// The call's own key.
    pub key: String,
    /// Display name: the call's `name:`, else the file's.
    pub name: String,
    /// Whether the call states `name:` (else the file's name is shown as
    /// inherited).
    pub name_stated: bool,
    /// Whether the call states `key:`.
    pub key_stated: bool,
    /// The `uses:` reference as written.
    pub source: String,
    /// The sequence file, root-relative when it is under the studio root,
    /// absolute otherwise (a package file: read-only in Studio).
    pub file: String,
    /// True when `file` is under the studio root and may be edited.
    pub editable: bool,
    /// False when the call is nested in a sequence file and its entry
    /// gives an `{inputs.x}` marker to a field that takes a flag or a
    /// choice (`enabled`, `then`): only the substituted entry types, so
    /// the name, `with:`, `plugs:`, `depends_on` and description shown
    /// are one caller's values. Read-only in Studio, like a phase whose
    /// `StudioPhaseSource.written_form` is false.
    pub written_form: bool,
    /// The call's own `description:`, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The sequence file's description, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_description: Option<String>,
    /// The list of its parent the call sits in: `setup`, `main` or
    /// `teardown`.
    pub stage: String,
    /// Position of the `uses:` entry in that list of the parent file
    /// (the procedure for a depth-1 call, the enclosing sequence file
    /// otherwise), counting phases and calls alike: what a code jump
    /// or a positional delete addresses. Always sent: a daemon old enough
    /// to lack it sends no `sequences` at all.
    pub entry_index: u32,
    /// `"slot"` or `"execution"` when the call states a `scope:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The entry's own `enabled:`, not derived from its phases.
    pub enabled: bool,
    /// The call's `depends_on` as written.
    pub depends_on: Vec<String>,
    /// The call's `then:` as `outcome -> action` pairs (`fail` ->
    /// `continue`).
    pub then: Vec<StudioSequencePlugConfigEntry>,
    /// The inputs the file declares, with the value this call gives
    /// (`with:`), the default, or neither.
    pub inputs: Vec<StudioSequenceInput>,
    /// The plug requirements the file declares, with the procedure plug
    /// key each is bound to (`plugs:` on the call, or the same key).
    pub plugs: Vec<StudioSequencePlugBinding>,
    /// Full keys of the phases this call holds, by the stage within the
    /// sequence they belong to (nested calls' phases included).
    pub setup_keys: Vec<String>,
    pub main_keys: Vec<String>,
    pub teardown_keys: Vec<String>,
}

/// One input of a sequence file as seen from a call.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceInput {
    pub name: String,
    /// `string`, `number`, `integer` or `boolean`.
    pub input_type: String,
    /// The value the call gives in `with:`, as JSON text; absent when the
    /// call relies on the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_json: Option<String>,
    /// The file's default, as JSON text; absent when the input is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One plug requirement of a sequence file as seen from a call.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequencePlugBinding {
    /// The requirement's key, the parameter name the file's phases use.
    pub key: String,
    pub name: String,
    /// The procedure plug key it is bound to.
    pub bound_to: String,
    /// Whether `plugs:` on the call states the binding (else same key).
    pub stated: bool,
    /// The class the file was written against, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequencePlug {
    pub key: String,
    pub name: String,
    pub python: String,
    /// `"slot"` | `"execution"` | `"station"` when the file states a
    /// `scope:` line, `None` when it does not and the engine's `slot`
    /// default applies. A String, not an enum, because this crate has
    /// no dependency on the engine's schema and the Builder only needs
    /// the canonical spelling to render a picker.
    ///
    /// The Option is the whole point: the runtime `PlugDefinition` has
    /// the default already applied, so it cannot say whether anyone
    /// chose it, and the Builder must not draw an inherited scope as a
    /// choice. Presence comes from `plugs_with_explicit_scope`, the
    /// spelling from the parsed definition — so a legacy `all` reports
    /// as stated, and reports as `"execution"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Option, not a skip-when-empty String: the generated TS would
    /// declare a required field the wire omits, and the page reads it
    /// as `?? ''`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `config:` keys in file order.
    ///
    /// Never `skip_serializing_if`: the wire always carries the list,
    /// empty included, which is the rule `StudioSequence.plugs` states —
    /// a list the wire omits generates a non-optional TS field and
    /// crashes the page on `undefined.map`.
    ///
    /// `serde(default)` is for the other direction, and it is why the
    /// generated TS declares this one optional: a Studio daemon older
    /// than the web app it is paired with sends a plug with no `config`
    /// at all, and the Builder has to read that as "no keys" rather
    /// than fail to parse the whole sequence.
    #[serde(default)]
    pub config: Vec<StudioSequencePlugConfigEntry>,
}

/// One `config:` key of a plug — a keyword argument the engine passes to
/// the plug class `__init__`.
///
/// A projection, not a round-trip: `value` is the scalar already
/// rendered for display, so the Builder never re-serializes YAML. `kind`
/// exists because the line editor can only rewrite a scalar in place; a
/// list or mapping value has to be sent to Code mode, and the page needs
/// to know that before it renders the row rather than after the write
/// fails.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequencePlugConfigEntry {
    pub key: String,
    /// The value as displayed: a string unquoted, a number or bool in
    /// its YAML spelling, and for `complex` a short rendering of the
    /// structure (`[3 items]`, `{2 keys}`) rather than the whole tree.
    pub value: String,
    /// `"string"` | `"number"` | `"bool"` | `"null"` | `"complex"`.
    /// A String rather than an enum, matching `scope` on the parent: the
    /// Builder only needs the canonical spelling to pick a write mode.
    pub kind: String,
    /// For `complex` only: the value itself as compact JSON. The plug
    /// debugger seeds session overrides from it — `value`'s summary is
    /// right for the Inspector (whose line editor cannot rewrite a
    /// structure in place) and wrong there. Absent for scalars, where
    /// `value` already IS the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_json: Option<String>,
}

/// Unit identification projection (`unit:` at the procedure root):
/// auto-identify, the operator-entered identity fields, and the
/// sub-unit list. Metadata stays in YAML.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceUnit {
    pub auto_identify: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<StudioSequenceUnitField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<StudioSequenceUnitField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_number: Option<StudioSequenceUnitField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_number: Option<StudioSequenceUnitField>,
    pub sub_units: Vec<StudioSequenceSubUnit>,
    /// The canonical identify-unit prompt, exactly as the engine's
    /// `identify_unit::build_components` will pose it at run time
    /// (serial + part always, then revision/batch/sub-units/metadata
    /// as configured). Lets Studio preview the real station screen
    /// without re-deriving the shape from the field configs above.
    /// `serde(default)` so payloads from older daemons still parse.
    #[serde(default)]
    pub components: Vec<UiComponent>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceSubUnit {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<StudioSequenceUnitField>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceUnitField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// `locked: true` — the value is the default, the operator cannot
    /// change it. Absent when false.
    // Attribute order matters to specta 1.0.5: it reads the serde list
    // left to right and a `skip_serializing_if` other than
    // `Option::is_none` resets the optional flag `default` sets, so
    // `default` must come last for the export to read `locked?: boolean`
    // (older daemons and hand-built fallbacks omit the key).
    #[serde(skip_serializing_if = "std::ops::Not::not", default = "default_false")]
    pub locked: bool,
}

/// A phase's `executable:` block, projected for the editor.
///
/// Separate from the `executable` flag next to it for the same reason
/// `ui_components` sits next to `ui`: the flag answers the tree's badge
/// in one byte, this answers the Inspector's fields. It also keeps the
/// flag's meaning stable for a dashboard talking to an older daemon,
/// which sends the flag and not this. The dashboard edits the block's
/// fields only when the block arrived, and reads the
/// `phase_executable` capability for the fallback message's wording
/// alone — switching a phase's runtime is deliberately not gated on
/// either (the trade-off is stated on the Runtime Select,
/// `phase-edit-page.tsx`).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceExecutable {
    /// May be empty: "runtime declared, command not chosen yet" is a
    /// legal state of the file. The engine loads it,
    /// `resolve_runtime_refs` reports it (Code's issues panel, the
    /// assistant), and the runner refuses to start on it — it is never
    /// refused at load.
    pub command: String,
    /// `bash|sh|zsh|powershell|pwsh|cmd`, absent when the file states
    /// none — the engine then defaults per platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequencePhase {
    pub key: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub measurements: Vec<StudioSequenceMeasurement>,
    pub depends_on: Vec<String>,
    /// Count of operator UI components (kept for the tree badges; the
    /// editable projection is `ui`).
    pub ui_components: u32,
    /// Phase runs an external executable (v2's shell equivalent).
    pub executable: bool,
    /// The `executable:` block itself, for the Inspector's fields.
    /// `None` on a Python or code-less phase — and also on any phase
    /// projected by a daemon predating the `phase_executable`
    /// capability, which is why an edit gates on the capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_config: Option<StudioSequenceExecutable>,
    /// Timeout in ms. u32 because the TS codegen forbids BigInt-mapped
    /// integers; the schema caps timeouts at 24h anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<StudioSequenceRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<StudioSequenceUi>,
    /// Instance keys of the sequence calls this phase came through; absent
    /// for a phase written in the procedure. The tree nests the phase
    /// under `StudioSequence::sequences[path]`, and an edit of it goes to
    /// that call's file, not the procedure's.
    // Always sent, empty included. `default` alone types it optional: a
    // daemon older than sequences omits it.
    #[serde(default)]
    pub sequence_path: Vec<String>,
    /// The sequence file and the entry's place in it, for a phase of a
    /// sequence: where the Inspector writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<StudioPhaseSource>,
}

/// Where a phase of a sequence was written.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioPhaseSource {
    /// Root-relative when under the studio root, absolute otherwise.
    pub file: String,
    /// `setup`, `main` or `teardown` in that file.
    pub list: String,
    /// Index of the entry in that list.
    pub index: u32,
    /// True when the file is under the studio root and may be edited.
    pub editable: bool,
    /// True when the entry is shown as the file writes it. False when the
    /// entry holds an `{inputs.x}` marker in a field that takes a number
    /// or a flag (a retry limit, a measurement limit): only the substituted
    /// entry types, the phase is shown as it runs, and an edit would write
    /// one caller's values into the shared file. Read-only in Studio.
    pub written_form: bool,
}

/// Retry policy projection (delay in ms, same u32 caveat as timeout).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceRetry {
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<u32>,
}

/// Operator UI projection. Components are the full runtime
/// `UiComponent` — the same type the operator UI renders — so the
/// Builder can edit every field the YAML declares (options, ranges,
/// image sizing, text styling) instead of a trimmed subset. `value` is
/// always absent here: nothing is running.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceUi {
    /// Explicit override; when absent the engine auto-detects from the
    /// component types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_input: Option<bool>,
    pub components: Vec<UiComponent>,
    /// Per component, index-aligned with `components`: whether the file
    /// writes a `key:` line, or the engine derived `key` from the label
    /// or the type. A component's `key` is always filled in, so it cannot
    /// tell the two apart, and the Builder must not draw a derived key as
    /// one somebody typed — clearing it would "reappear", and editing the
    /// label silently rewrites it.
    ///
    /// Here and not on `UiComponent`: that type is the runtime's, shared
    /// with the operator UI, and this describes the file for the editor
    /// alone. Absent from a daemon that predates the
    /// `component_key_stated` capability (`serde(default)` so its payload
    /// still parses) — the Builder gates on the capability, never on
    /// this being non-empty.
    #[serde(default)]
    pub keys_stated: Vec<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceMeasurement {
    pub key: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub validators: Vec<StudioSequenceValidator>,
    /// Measurement-level aggregations declared in the YAML.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<StudioSequenceAggregation>,
    /// Multi-dimensional chart title, when the measurement declares axes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_axis: Option<StudioSequenceAxis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub y_axis: Vec<StudioSequenceAxis>,
}

/// Declared aggregation on a measurement or axis. `type` is the free-form
/// label the test code fills at runtime ("mean", "min", ...); only the
/// declaration is editable here, values exist only in run results.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceAggregation {
    #[serde(rename = "type")]
    pub aggregation_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<StudioSequenceValidator>,
}

/// One axis of a multi-dimensional measurement as declared in the YAML.
/// `key` and `legend` are the resolved values (the engine derives one
/// from the other when only one is written).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceAxis {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<StudioSequenceAggregation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<StudioSequenceValidator>,
}

/// Display form of a validator: operator + value already rendered to
/// strings ("expression" validators carry the expression in `detail`).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioSequenceValidator {
    pub detail: String,
}

/// One entry in a `StudioResponse::Files` listing.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioFileEntry {
    pub name: String,
    /// Root-relative path, `/`-separated on all platforms.
    pub path: String,
    pub kind: StudioEntryKind,
    /// File size in bytes, saturated to u32 (display-only; the read
    /// path enforces its own byte cap). u32 because the TS codegen
    /// forbids BigInt-mapped integers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StudioEntryKind {
    File,
    Dir,
}

/// Where a `VenvBootstrap` event sits in the build.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum VenvBootstrapStage {
    Started,
    Finished,
}

/// Which file `PythonEnv` found the procedure's dependencies in. An
/// enum rather than a string so the TypeScript export is the closed
/// union a client can branch on: the daemon's detection and the
/// browser's reading of it cannot drift apart.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StudioDepsSource {
    /// A `pyproject.toml`, in the procedure's directory or an
    /// ancestor (uv workspace).
    Pyproject,
    /// A `requirements.txt` next to the procedure.
    Requirements,
    /// Neither: the venv was provisioned with no third-party package.
    None,
}

/// A validation finding surfaced to the Studio UI.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioDiagnostic {
    pub severity: StudioDiagnosticSeverity,
    pub message: String,
    /// Root-relative file the finding is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The YAML entry the finding points at, when the lint knows it.
    /// The web resolves it to a line with the same helper the Sequence
    /// view uses for its own jumps, so an Issues click lands on the
    /// entry rather than on a guess mined from the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<StudioDiagnosticLocation>,
    /// 1-based position, when the source has one (a YAML parse error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Stable kind of finding (`plug_class_not_found`, `parse_error`…),
    /// the same `code` `tofupilot validate --json` prints. Absent from a
    /// daemon older than CLI 2.10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The replacement the message suggests (`plugs.psu:PowerSupply`), for
    /// a quick fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// A top-level list of the procedure YAML — the web's `EntryLocation.list`.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StudioDiagnosticList {
    Plugs,
    Setup,
    Main,
    Teardown,
}

/// An entry of a top-level list by position, and optionally the key
/// inside it (`python`, `executable.command`, `config.<name>`).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StudioDiagnosticLocation {
    pub list: StudioDiagnosticList,
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StudioDiagnosticSeverity {
    Error,
    Warning,
}

/// Typed failure for a Studio request. `Unsupported` is the
/// forward-compat arm: an older daemon answers ops it doesn't know
/// with it instead of dropping the request.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StudioErrorCode {
    NotFound,
    Forbidden,
    Conflict,
    TooLarge,
    Invalid,
    Unsupported,
    Internal,
    /// The session is doing something that the request would corrupt.
    /// Today: switching project while a run is in flight — the run
    /// captured its root at start, so moving the active project under
    /// it would have the UI and the engine describing two projects.
    /// Also a second `PickProject` while a picker window is open:
    /// dialogs must not stack (reentrance, not security).
    Busy,
    /// The human's decision, not a failure: the dialog a request opened
    /// was dismissed choosing nothing (the UI returns to its previous
    /// state without comment), or a plug debug stop landed during the
    /// open or the call it answers (the plug was released).
    Cancelled,
    /// The session holds no project (see `StudioResponse::NoProject`),
    /// so a request that needs a root to resolve against has nothing to
    /// work on. Distinct from `NotFound`: the path is not missing, the
    /// project is. The page's welcome state never sends such a request;
    /// a stale tab or a second client can.
    NoProject,
}

// ---------------------------------------------------------------------------
// Shared sub-types
// ---------------------------------------------------------------------------

/// A planned phase (name known upfront, outcome unknown).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PhasePlan {
    pub key: String,
    pub name: String,
    /// Stage scope — `"setup_all"`, `"setup_each"`, `"main"`, `"teardown_each"`,
    /// `"teardown_all"`. Empty for pre-scope emitters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    /// Instance keys of the sequence calls the phase came through, root
    /// first (`["calib", "dmm"]` for key `calib/dmm/measure`). Empty for a
    /// phase written in the procedure, absent from emitters that predate
    /// sequences. Consumers group the plan by it, matching
    /// `RunStarted.sequences[].path`.
    // Always sent, empty included. `default` alone types it optional:
    // older emitters omit it.
    #[serde(default)]
    pub sequence_path: Vec<String>,
}

/// A sequence call of the run, advertised on `RunStarted.sequences`
/// (parents before children) so consumers can draw the group before its
/// `SequenceComplete` lands. `path.join("/")` is the dashboard's
/// `sequence_path`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SequencePlan {
    /// Instance keys from the procedure root to this call.
    pub path: Vec<String>,
    /// The call's own key (last element of `path`).
    pub key: String,
    /// Display name of the call.
    pub name: String,
    /// The `uses:` reference as written.
    pub source: String,
    /// The list of its parent the call sits in: `setup`, `main` or
    /// `teardown`.
    pub stage: String,
    /// `"slot"` or `"execution"` on a call made from the procedure's
    /// setup or teardown; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Plug definition advertised on `RunStarted.plugs`. Lets consumers
/// know which plugs the procedure declared before any `plug_status`
/// or `plug_log` event lands, so they can render an empty-state row
/// (`pending`) and never have to upsert a fresh entry on the fly.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PlugDefinition {
    /// Stable key (e.g. `"dmm"`). Matches `plug_status.plug_key` and
    /// `plug_log.plug_key` for downstream identity.
    pub key: String,
    /// Display name as configured in the procedure.
    pub name: String,
    /// `"slot"` | `"execution"` | `"station"`.
    /// `slot` plugs are instantiated per slot; `execution` plugs once
    /// per execution (all slots); `station` plugs once per station
    /// process, held across executions.
    pub scope: String,
}

/// Unit metadata captured before a run starts. Optional inside
/// `RunStarted.unit` because runs that prompt for identification at
/// runtime resolve the unit via the identify-unit `UiRequest`
/// instead. Field names mirror the dashboard's run table columns
/// so consumers can render without remapping.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UnitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_number: Option<String>,
    /// Sub-unit slots keyed by slot name. Empty when the procedure
    /// doesn't declare sub-units.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub sub_units: std::collections::HashMap<String, String>,
}

/// A single captured log line from a phase attempt.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PhaseLogLine {
    pub level: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Source filename the log line originated from (relative to the
    /// procedure dir). Optional because some emitters — notably plug
    /// log forwards — don't carry source context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// A measurement result emitted with PhaseComplete.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunMeasurement {
    pub name: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    /// Per-validator results as evaluated at phase completion. Populated
    /// when the engine has validator metadata (YAML-defined measurements);
    /// left empty for ad-hoc measurements without declared validators.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorResult>,
    /// Measurement-level aggregations (mean/min/max/...), evaluated at
    /// phase completion. Axis-level aggregations of multi-dimensional
    /// measurements travel inside `measured_value` instead — this field
    /// carries only the ones attached directly to the measurement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<AggregationResult>,
    /// The range the declared limits draw: the tightest `>` / `>=` and
    /// `<` / `<=`. For displays (an operator panel's gauge band); the
    /// verdict is `validators`'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<f64>,
}

/// One evaluated aggregation attached to a `RunMeasurement`. The type is
/// a free-form string ("mean", "min", ...) — the engine never computes
/// values, it only evaluates the validators against the caller-supplied
/// value and rolls up the outcome.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AggregationResult {
    #[serde(rename = "type")]
    pub aggregation_type: String,
    /// Caller-supplied value: number, string or boolean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// `"PASS"` / `"FAIL"` / `"UNSET"` — same vocabulary as the outer
    /// measurement outcome.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorResult>,
}

/// One validator outcome attached to a `RunMeasurement`. Mirrors the web
/// `ValidatorInfo` schema but kept minimal for the live wire payload.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ValidatorResult {
    /// Human-readable expression for display (e.g. "x >= 3.0").
    pub expression: String,
    /// `"PASS"` / `"FAIL"` / `"UNSET"` — same vocabulary as the outer
    /// measurement outcome.
    pub outcome: String,
    /// If false, a FAIL here doesn't flip the measurement outcome
    /// (indicative/marginal validator). Null when the engine couldn't
    /// determine decisiveness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_decisive: Option<bool>,
}

/// An operator panel: one HTML file (inline SVG for instruments) bound
/// to the phase's values by `data-bind` and standard `name=` controls.
/// The station reads the file at procedure load and ships its text, so a
/// viewer without the procedure directory (the dashboard) renders the
/// same screen. Untrusted markup: every surface sanitizes it before
/// rendering.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiPanel {
    pub html: String,
    /// Hex SHA-256 of `html`. Lets a surface keep its mounted panel
    /// across a replay of the same prompt instead of re-parsing it.
    pub sha256: String,
    /// The `<button name>`s of the panel that are neither a submit nor
    /// the run's `stop` / `kill`: what a surface may press
    /// (`StationCommand::UiEvent`) and the phase reads with
    /// `ui.pressed(name)`. Scanned by the station; a surface without the
    /// HTML (the TUI, an agent) lists them from here.
    #[serde(default)]
    pub events: Vec<String>,
}

/// Operator-UI component, single source of truth across the engine,
/// the Centrifugo wire, the local-websocket wire, and the operator-UI
/// React renderer.
///
/// Why `value` lives on the wire alongside `ui_update`: the engine
/// emits a `UiRequest` once at phase start (with `value: None`) and
/// then per-component `UiUpdate{set_value}` deltas as Python mutates
/// `ui.<key> = v`. Clients fold the deltas into their cached
/// `UiRequest` (see `applyUiUpdateToRequest` in
/// `packages/operator-ui/src/state/ui-requests.ts`), so a hydration replay
/// from any prefix of the event log renders the latest value. The
/// field is on the struct — not just on `UiUpdate` — so a snapshot
/// taken after a `set_value` can re-emit a self-sufficient
/// `UiRequest` without depending on `ui_update` event-order replay.
///
/// `is_input` is materialized at construction (see `UiComponent::new`)
/// so operator-UI consumers don't reimplement the input-vs-display
/// classification — adding a new `ComponentType` variant only updates
/// `ComponentType::is_input` and the rest of the stack reads it.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiComponent {
    pub key: String,
    #[serde(rename = "type")]
    pub component_type: ComponentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub is_input: bool,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UiOption>>,
    /// Default value declared in YAML. Pre-fills the input at request time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<ComponentValue>,
    /// Runtime value: `None` at phase start, populated by clients
    /// folding `UiUpdate{set_value}` deltas into their cached
    /// `UiRequest` so a hydration replay carries the latest value
    /// without depending on `ui_update` event-order replay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ComponentValue>,
    /// Bind component value to a measurement (`measurements.X`) or unit
    /// field (`unit.X`). Without this on the wire, the web submit can't
    /// build the `__bound_measurements__` payload the engine consumes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    // Image and image-grid (radio/checklist with image options) sizing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>,
    // Text input validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Author-written error shown when the value fails `pattern`.
    /// Without it, clients derive a message from charset-only patterns
    /// and fall back to a generic one for structural patterns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_message: Option<String>,
    // Textarea-specific.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    // Text input affordances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default = "default_trim")]
    pub trim: bool,
    /// Read-only: rendered dimmed and disabled, skipped by the focus
    /// rule and by `validate::validate_response`. Only the engine's
    /// identify builder sets it, for a `unit:` field with
    /// `locked: true` — phase `ui.components` cannot declare it. The
    /// engine fills the field from the procedure config and ignores
    /// any value submitted under its key.
    // Attribute order matters to specta 1.0.5: it reads the serde list
    // left to right and a `skip_serializing_if` other than
    // `Option::is_none` resets the optional flag `default` sets, so
    // `default` must come last for the export to read `locked?: boolean`
    // (older daemons and hand-built fallbacks omit the key).
    #[serde(skip_serializing_if = "std::ops::Not::not", default = "default_false")]
    pub locked: bool,
    // Text styling (display components).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<TextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<TextColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontFamily>,
}

fn default_trim() -> bool {
    true
}

/// serde default for `locked` (see the attribute-order note on the field).
fn default_false() -> bool {
    false
}

impl UiComponent {
    /// Construct a component with `is_input` derived from the type.
    /// All optional fields default to `None` / sensible defaults; use
    /// struct-update syntax (`UiComponent { key: …, ..UiComponent::new(t) }`)
    /// or set fields after the call.
    pub fn new(component_type: ComponentType) -> Self {
        Self {
            key: String::new(),
            component_type,
            label: None,
            is_input: component_type.is_input(),
            required: true,
            description: None,
            placeholder: None,
            options: None,
            default_value: None,
            value: None,
            bind: None,
            min: None,
            max: None,
            step: None,
            columns: None,
            width: None,
            height: None,
            aspect: None,
            fit: None,
            min_length: None,
            max_length: None,
            pattern: None,
            pattern_message: None,
            rows: None,
            prefix: None,
            suffix: None,
            trim: true,
            locked: false,
            size: None,
            color: None,
            font: None,
        }
    }
}

/// Type of UI component. Wire format is snake_case (`text_input`,
/// `number_input`, …). Adding a variant here forces a recompile of
/// every consumer that matches on it.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    // Input components (require user interaction)
    TextInput,
    NumberInput,
    Switch,
    Textarea,
    Radio,
    Select,
    Multiselect,
    Checklist,
    Slider,

    // Display components (output only)
    Text,
    Image,
    Progress,
}

impl ComponentType {
    /// True for component types that require operator interaction.
    /// Source of truth for the `is_input` field on `UiComponent` and
    /// for the operator-UI's input-vs-display filter.
    pub const fn is_input(self) -> bool {
        matches!(
            self,
            ComponentType::TextInput
                | ComponentType::NumberInput
                | ComponentType::Textarea
                | ComponentType::Radio
                | ComponentType::Select
                | ComponentType::Multiselect
                | ComponentType::Checklist
                | ComponentType::Switch
                | ComponentType::Slider
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ComponentType::TextInput => "text_input",
            ComponentType::NumberInput => "number_input",
            ComponentType::Textarea => "textarea",
            ComponentType::Radio => "radio",
            ComponentType::Select => "select",
            ComponentType::Multiselect => "multiselect",
            ComponentType::Checklist => "checklist",
            ComponentType::Switch => "switch",
            ComponentType::Slider => "slider",
            ComponentType::Text => "text",
            ComponentType::Image => "image",
            ComponentType::Progress => "progress",
        }
    }

    /// Whether anything reads `field` on this component type. Fields the
    /// table does not list are read everywhere (`key`, `label`, ...).
    pub fn reads(self, field: &str) -> bool {
        COMPONENT_FIELD_READERS
            .iter()
            .find(|(name, _)| *name == field)
            .is_none_or(|(_, readers)| readers.contains(&self))
    }
}

/// Which component types read each type-specific field. Anywhere else
/// the field loads, renders nothing and validates nothing, which is what
/// the procedure lint warns about (`UIComponent::inert_fields`).
///
/// THE table: written once, here. `export-types` emits it into
/// `station-protocol.ts`, where the Studio agent's schema reference is
/// built from it and a test holds it against what the web renderers
/// actually read — so it is never retyped, and cannot drift unnoticed.
///
/// A type is listed for a field as soon as ONE reader uses it there:
///   - the web renderers (`packages/shared/components/operator-ui/
///     components/*.tsx`). `radio` and `checklist` hand image options to
///     `ImageGridInput`, hence the image layout fields and `columns`;
///     `switch` reads `options` for its two labels.
///   - response validation (`validate.rs` here, `validation.ts` on the
///     web): length and pattern rules apply to `textarea` too, though
///     its renderer never mentions them.
///   - the kiosk TUI, which reads a subset of the above.
///
/// Left out on purpose, because a finding must never be wrong:
/// `required` and `trim` are plain bools (stated and defaulted look the
/// same, and `trim` applies to every input), `default_value` and `bind`
/// have rules of their own.
pub const COMPONENT_FIELD_READERS: &[(&str, &[ComponentType])] = {
    use ComponentType as T;
    const TEXT_ENTRY: &[T] = &[T::TextInput, T::Textarea];
    const IMAGE_LAYOUT: &[T] = &[T::Image, T::Radio, T::Checklist];
    &[
        (
            "placeholder",
            &[
                T::TextInput,
                T::NumberInput,
                T::Textarea,
                T::Select,
                T::Multiselect,
                T::Image,
                T::Radio,
                T::Checklist,
            ],
        ),
        ("min_length", TEXT_ENTRY),
        ("max_length", TEXT_ENTRY),
        ("pattern", TEXT_ENTRY),
        ("pattern_message", TEXT_ENTRY),
        ("prefix", &[T::TextInput, T::NumberInput]),
        ("suffix", &[T::TextInput, T::NumberInput]),
        ("rows", &[T::Textarea]),
        ("min", &[T::NumberInput, T::Slider, T::Progress]),
        ("max", &[T::NumberInput, T::Slider, T::Progress]),
        ("step", &[T::NumberInput, T::Slider]),
        (
            "options",
            &[T::Radio, T::Select, T::Multiselect, T::Checklist, T::Switch],
        ),
        ("columns", &[T::Radio, T::Checklist]),
        ("width", IMAGE_LAYOUT),
        ("height", IMAGE_LAYOUT),
        ("aspect", IMAGE_LAYOUT),
        ("fit", IMAGE_LAYOUT),
        ("size", &[T::Text]),
        ("color", &[T::Text]),
        ("font", &[T::Text]),
    ]
};

/// Runtime value carried by a `UiComponent`. Untagged so the wire
/// matches the JSON literal shape the operator-UI submits and Python
/// `ui.<key> = v` produces.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ComponentValue {
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<String>),
}

/// Text size token. Wire format mirrors Tailwind's text-size scale so
/// a YAML literal like `size: 2xl` round-trips end-to-end.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum TextSize {
    #[serde(rename = "xs")]
    Xs,
    #[serde(rename = "sm")]
    Sm,
    #[serde(rename = "base")]
    Base,
    #[serde(rename = "lg")]
    Lg,
    #[serde(rename = "xl")]
    Xl,
    #[serde(rename = "2xl")]
    Xl2,
    #[serde(rename = "3xl")]
    Xl3,
    #[serde(rename = "4xl")]
    Xl4,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum TextColor {
    Default,
    Zinc,
    Red,
    Orange,
    Yellow,
    Green,
    Lime,
    Sky,
    Blue,
    Violet,
    Purple,
    Pink,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FontFamily {
    Default,
    Monospace,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UiOption {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// Operator-runtime screen states. Both the web operator UI (React)
/// and the CLI kiosk TUI (Ratatui) derive their current screen from
/// the same precedence ladder against `(runState, identifyRequest,
/// committedProcedureId, unlinkedProcedureName, focusedPhase)`.
/// Promoting the enum into the shared protocol crate keeps the two
/// runtimes in lockstep — adding a new screen state here forces the
/// TS reducer and the Rust TUI's render branch to acknowledge it.
///
/// Precedence (highest first):
///   1. `ProcedureUnlinked` — committed procedure was removed mid-flow.
///   2. `Idle` — no commit; show picker.
///   3. `IdentifyUnit` — pre-run operator identify prompt active.
///   4. `Starting` — `runState.executionId == "pending"` (handleRun seed
///      before `RunStarted` lands).
///   5. `Outcome` — `runState.outcome` set (terminal screen).
///   6. `Running` — `runState` + a focused phase (active or last
///      completed).
///   7. `Waiting` — `runState` exists but no focused phase yet.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum Screen {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "identify-unit")]
    IdentifyUnit,
    #[serde(rename = "waiting")]
    Waiting,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "outcome")]
    Outcome,
    #[serde(rename = "procedure-unlinked")]
    ProcedureUnlinked,
}

/// Why an upload attempt failed. Drives operator-UI bucketing of the
/// queue row's status — `Http4xx` is parked (won't auto-retry), the
/// rest are transient with a `next_retry_at`. Wire format is
/// snake_case lower (`http_4xx` etc.) to match the existing `kind`
/// strings producers ship.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RunUploadFailedKind {
    #[serde(rename = "http_4xx")]
    Http4xx,
    #[serde(rename = "http_5xx")]
    Http5xx,
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "unknown")]
    Unknown,
}

/// Why a queue entry was dropped. `Manual` means the operator clicked
/// Drop. `Ttl` / `Invalid` are reserved (CLI doesn't auto-drop today)
/// — kept on the wire so future enforcement doesn't need a protocol
/// bump. Wire format mirrors existing string literals.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "lowercase")]
pub enum RunUploadDroppedReason {
    Manual,
    Ttl,
    Invalid,
}

/// Why a `Logout` was issued. `User` = operator-initiated from the
/// dashboard. `Replaced` = a fresh install took over the
/// installation_id. `Uninstalled` = backend marked the station
/// uninstalled. Wire format is snake_case to match existing strings.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum LogoutReason {
    User,
    Replaced,
    Uninstalled,
}

/// Phase lifecycle stage. Wire format mirrors existing string
/// literals so the typed enum can replace `String` in producers
/// over time without a breaking wire change.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PhaseStage {
    SetupAll,
    SetupEach,
    Main,
    TeardownEach,
    TeardownAll,
}

/// Plug lifecycle stage scope. `Setup` and `Teardown` mirror the
/// engine's plug lifecycle; `Manual` is for plugs invoked outside
/// the standard setup/teardown lattice.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PlugStage {
    Setup,
    Teardown,
    Manual,
}

/// Plug lifecycle status. Mirrors the engine's `PlugStatusValue`
/// plus a `Pending` placeholder consumers use until the first
/// `plug_status` event lands.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PlugLifecycleStatus {
    Pending,
    Idle,
    Initializing,
    Active,
    Destructing,
    Error,
    Skipped,
}

/// Measurement validator outcome. Mirrors the engine's per-validator
/// results and matches the outer `RunMeasurement.outcome` vocabulary.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "UPPERCASE")]
pub enum MeasurementOutcome {
    Pass,
    Fail,
    Unset,
}

/// Run-level terminal outcome. `Skip` is reserved for procedures that
/// return early without phase failures; `Error` is engine-level
/// failure (load, init, subprocess crash); `Aborted` is operator-
/// initiated `Stop` / `Kill`.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "UPPERCASE")]
pub enum RunOutcome {
    Pass,
    Fail,
    Error,
    Aborted,
    Skip,
}

/// Mid-prompt mutation action. `set_value` carries `{id, value}`
/// (`ui.<key> = value`); `set_properties` carries a
/// [`UiSetPropertiesPayload`] (`ui.set(key, …)`). The open-set was
/// leaving room for `add_component` / `add_log`. Kept as an enum so a
/// producer typo fails to deserialize (and an unknown future variant
/// from a newer CLI is rejected by older operator UIs).
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UiUpdateAction {
    SetValue,
    SetProperties,
}

/// Sentinel `executionId` for the operator-UI's pending-seed RunState.
/// Mirror of `PENDING_EXECUTION_ID` in
/// `packages/operator-ui/src/state/run-state-types.ts`. Both runtimes treat events
/// stamped against this id as "accepted, not stale" — see
/// `is_stale_for_execution`. Never appears on the wire; lives in the
/// shared crate so a Rust-side reducer can apply the same gate.
pub const PENDING_EXECUTION_ID: &str = "pending";

impl StationEvent {
    /// Per-run identity stamped on this event, when it carries one.
    /// Returns `None` for station-lifecycle events (Hardware,
    /// Telemetry, Deployment*, Update*, Config*, Presence) and for
    /// older emitters that predate the `execution_id` field.
    pub fn execution_id(&self) -> Option<&str> {
        match self {
            Self::RunStarted { execution_id, .. } => Some(execution_id.as_str()),
            Self::PhaseStarted { execution_id, .. }
            | Self::PhaseLog { execution_id, .. }
            | Self::MeasurementUpdate { execution_id, .. }
            | Self::AttachmentAdded { execution_id, .. }
            | Self::PhaseComplete { execution_id, .. }
            | Self::SequenceComplete { execution_id, .. }
            | Self::RunComplete { execution_id, .. }
            | Self::PlugStatus { execution_id, .. }
            | Self::PlugLog { execution_id, .. }
            | Self::UiRequest { execution_id, .. }
            | Self::UiUpdate { execution_id, .. }
            | Self::IdentifyRequest { execution_id, .. }
            | Self::IdentifyResolved { execution_id, .. }
            | Self::IdentifyTimeout { execution_id, .. }
            | Self::RunCrashed { execution_id, .. } => execution_id.as_deref(),
            _ => None,
        }
    }
}

impl UnitInfo {
    /// Field-level merge: non-null fields on `update` overwrite, sub-units
    /// merge by key. Mirrors the wire contract documented at
    /// `IdentifyResolved` and the TS `mergeUnit` helper. Promoted to
    /// the protocol crate so a Rust-side reducer can apply identical
    /// semantics — previously the contract lived only in TS.
    pub fn merge(&mut self, update: &UnitInfo) {
        if update.serial_number.is_some() {
            self.serial_number = update.serial_number.clone();
        }
        if update.part_number.is_some() {
            self.part_number = update.part_number.clone();
        }
        if update.revision_number.is_some() {
            self.revision_number = update.revision_number.clone();
        }
        if update.batch_number.is_some() {
            self.batch_number = update.batch_number.clone();
        }
        for (k, v) in &update.sub_units {
            self.sub_units.insert(k.clone(), v.clone());
        }
    }
}

/// Inputs the `Screen` derivation reads. Decoupled from any concrete
/// `RunState` shape so both the TS reducer and a Rust TUI can build
/// it from their own structures and call the shared derivation.
#[derive(Debug, Clone, Copy)]
pub struct ScreenInputs<'a> {
    /// `true` when the operator is committed to a procedure (clicked
    /// Run, mid-identify, or hydrated mid-run).
    pub committed: bool,
    /// `true` when an identify-unit prompt is in flight.
    pub identify_active: bool,
    /// `true` when a `RunState` exists and its `executionId` equals
    /// `PENDING_EXECUTION_ID` — the synchronous seed planted on
    /// handleRun before the engine's `RunStarted` arrives.
    pub run_pending: bool,
    /// `true` when a `RunState` exists at all (live, terminal, or
    /// pending). Mirrors `runState != null`.
    pub run_active: bool,
    /// `true` when the active run carries a non-null `outcome` (PASS,
    /// FAIL, ERROR, ABORTED, SKIP).
    pub run_terminal: bool,
    /// `true` when a focused phase exists for the running screen.
    pub focused_phase: bool,
    /// `true` when the committed procedure was unlinked mid-flow.
    pub unlinked: bool,
    /// Phantom lifetime so callers can borrow without forcing
    /// `'static`. Not currently read.
    pub _phantom: std::marker::PhantomData<&'a ()>,
}

/// Derive the operator-UI screen from the runtime inputs. Single
/// source of truth for the precedence ladder — both the TS hook
/// (`use-operator-ui-state.ts::screen`) and the Rust TUI must call
/// this to stay in lockstep on which screen to render.
pub fn derive_screen(inputs: ScreenInputs<'_>) -> Screen {
    if inputs.unlinked {
        return Screen::ProcedureUnlinked;
    }
    if !inputs.committed {
        return Screen::Idle;
    }
    if inputs.identify_active {
        return Screen::IdentifyUnit;
    }
    if inputs.run_pending {
        return Screen::Starting;
    }
    if inputs.run_terminal {
        return Screen::Outcome;
    }
    if inputs.run_active && inputs.focused_phase {
        return Screen::Running;
    }
    if inputs.run_active {
        return Screen::Waiting;
    }
    Screen::Starting
}

/// Cross-run gate predicate. Returns `true` when an event's
/// `execution_id` doesn't match the active run's id and both are
/// known — caller drops the event to avoid corrupting the current
/// run's state with leftovers from a cancelled prior run. Returns
/// `false` (event is OK) when:
///   * The active id is missing or `PENDING_EXECUTION_ID` (pre-run
///     seed) — every event is accepted into the gap.
///   * The event has no `execution_id` (older emitters /
///     station-lifecycle events).
///   * The two ids match.
///
/// Mirror of `isStaleForExecution` in
/// `packages/operator-ui/src/state/run-state.ts`. Same semantics; promoted
/// here so a Rust reducer can apply the gate without re-deriving it.
pub fn is_stale_for_execution(active: Option<&str>, event: Option<&str>) -> bool {
    let Some(active) = active else { return false };
    if active.is_empty() || active == PENDING_EXECUTION_ID {
        return false;
    }
    let Some(event) = event else { return false };
    event != active
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Screen serialisation must match the TS reducer's string literals
    /// 1:1. The TS side imports `Screen` from
    /// `packages/shared/types/generated/station-protocol.ts`
    /// (regenerated by `cargo run --features specta --bin export-types`); a rename here
    /// without a regen + re-deploy of the operator UI would split the
    /// runtimes.
    #[test]
    fn is_stale_drops_cross_run_event() {
        assert!(is_stale_for_execution(Some("a"), Some("b")));
    }

    #[test]
    fn is_stale_accepts_match() {
        assert!(!is_stale_for_execution(Some("a"), Some("a")));
    }

    #[test]
    fn is_stale_accepts_pending_seed() {
        assert!(!is_stale_for_execution(
            Some(PENDING_EXECUTION_ID),
            Some("anything")
        ));
    }

    #[test]
    fn is_stale_accepts_no_active() {
        assert!(!is_stale_for_execution(None, Some("anything")));
    }

    #[test]
    fn is_stale_accepts_eventless() {
        assert!(!is_stale_for_execution(Some("a"), None));
    }

    #[test]
    fn execution_id_extracts_from_run_started() {
        let ev = StationEvent::RunStarted {
            procedure_id: "p".into(),
            procedure_name: String::new(),
            execution_id: "e".into(),
            phases: vec![],
            slots: vec![],
            slot_names: Default::default(),
            slot_units: Default::default(),
            plugs: vec![],
            timestamp: None,
            run_id: None,
            deployment_id: None,
            unit: None,
            only_phase: None,
            procedure_version: None,
            procedure_description: None,
            sequences: vec![],
            panel: None,
        };
        assert_eq!(ev.execution_id(), Some("e"));
    }

    // The sequence lists go on the wire empty: a reader typed by the
    // generated TS must find them on every event from this emitter.
    #[test]
    fn empty_sequence_lists_are_sent() {
        let ev = StationEvent::RunStarted {
            procedure_id: "p".into(),
            procedure_name: String::new(),
            execution_id: "e".into(),
            phases: vec![PhasePlan {
                key: "a".into(),
                name: "A".into(),
                stage: "main".into(),
                sequence_path: vec![],
            }],
            slots: vec![],
            slot_names: Default::default(),
            slot_units: Default::default(),
            plugs: vec![],
            timestamp: None,
            run_id: None,
            deployment_id: None,
            unit: None,
            only_phase: None,
            procedure_version: None,
            procedure_description: None,
            sequences: vec![],
            panel: None,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["sequences"], serde_json::json!([]), "{json}");
        assert_eq!(
            json["phases"][0]["sequence_path"],
            serde_json::json!([]),
            "{json}"
        );
        // An older emitter's event, without them, still reads.
        let older = serde_json::json!({
            "type": "run_started", "procedure_id": "p", "procedure_name": "", "execution_id": "e",
            "phases": [{"key": "a", "name": "A", "stage": "main"}], "slots": [],
            "slot_names": {}, "slot_units": {}, "plugs": []
        });
        let read: StationEvent = serde_json::from_value(older).unwrap();
        let StationEvent::RunStarted {
            sequences, phases, ..
        } = read
        else {
            panic!("run_started")
        };
        assert!(sequences.is_empty() && phases[0].sequence_path.is_empty());
    }

    #[test]
    fn execution_id_none_for_lifecycle() {
        let ev = StationEvent::Hardware {
            installation_id: "i".into(),
            hostname: "h".into(),
            os: "o".into(),
            platform: "p".into(),
            mac_address: None,
            cli_version: "v".into(),
            run_mode: None,
        };
        assert_eq!(ev.execution_id(), None);
    }

    #[test]
    fn merge_overwrites_non_null_fields() {
        let mut base = UnitInfo {
            serial_number: Some("SN1".into()),
            part_number: Some("P1".into()),
            revision_number: None,
            batch_number: None,
            sub_units: Default::default(),
        };
        let update = UnitInfo {
            serial_number: None,
            part_number: Some("P2".into()),
            revision_number: Some("R1".into()),
            batch_number: None,
            sub_units: Default::default(),
        };
        base.merge(&update);
        assert_eq!(base.serial_number.as_deref(), Some("SN1"));
        assert_eq!(base.part_number.as_deref(), Some("P2"));
        assert_eq!(base.revision_number.as_deref(), Some("R1"));
        assert_eq!(base.batch_number, None);
    }

    fn inputs() -> ScreenInputs<'static> {
        ScreenInputs {
            committed: false,
            identify_active: false,
            run_pending: false,
            run_active: false,
            run_terminal: false,
            focused_phase: false,
            unlinked: false,
            _phantom: std::marker::PhantomData,
        }
    }

    #[test]
    fn screen_idle_default() {
        assert_eq!(derive_screen(inputs()), Screen::Idle);
    }

    #[test]
    fn screen_unlinked_overrides() {
        let mut i = inputs();
        i.unlinked = true;
        i.run_active = true;
        i.run_terminal = true;
        assert_eq!(derive_screen(i), Screen::ProcedureUnlinked);
    }

    #[test]
    fn screen_identify_beats_pending() {
        let mut i = inputs();
        i.committed = true;
        i.identify_active = true;
        i.run_pending = true;
        assert_eq!(derive_screen(i), Screen::IdentifyUnit);
    }

    #[test]
    fn screen_outcome_when_terminal() {
        let mut i = inputs();
        i.committed = true;
        i.run_active = true;
        i.run_terminal = true;
        assert_eq!(derive_screen(i), Screen::Outcome);
    }

    #[test]
    fn screen_running_when_focused_phase() {
        let mut i = inputs();
        i.committed = true;
        i.run_active = true;
        i.focused_phase = true;
        assert_eq!(derive_screen(i), Screen::Running);
    }

    #[test]
    fn screen_waiting_when_active_no_focus() {
        let mut i = inputs();
        i.committed = true;
        i.run_active = true;
        assert_eq!(derive_screen(i), Screen::Waiting);
    }

    #[test]
    fn screen_wire_format() {
        let cases = [
            (Screen::Idle, "\"idle\""),
            (Screen::Starting, "\"starting\""),
            (Screen::IdentifyUnit, "\"identify-unit\""),
            (Screen::Waiting, "\"waiting\""),
            (Screen::Running, "\"running\""),
            (Screen::Outcome, "\"outcome\""),
            (Screen::ProcedureUnlinked, "\"procedure-unlinked\""),
        ];
        for (screen, expected) in cases {
            let actual = serde_json::to_string(&screen).expect("serialize");
            assert_eq!(actual, expected, "wire format for {screen:?}");
            let round: Screen = serde_json::from_str(&actual).expect("deserialize");
            assert_eq!(round, screen, "round-trip for {screen:?}");
        }
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn stamped_command_parses_as_envelope_and_as_bare_command() {
        let wire = r#"{"type":"ui_response","request_id":"r1","values":{"sn":"A1"},
            "command_id":"c1","actor":{"kind":"user","user_id":"u1","display_name":"Ada"}}"#;
        let env: CommandEnvelope = serde_json::from_str(wire).unwrap();
        assert_eq!(env.command_id.as_deref(), Some("c1"));
        match env.into_command() {
            StationCommand::UiResponse(answer) => {
                assert_eq!(answer.request_id, "r1");
                assert_eq!(answer.actor.unwrap().display_name, "Ada");
            }
            other => panic!("unexpected {other:?}"),
        }
        // A CLI that predates the stamp still reads the command.
        let bare: StationCommand = serde_json::from_str(wire).unwrap();
        assert!(matches!(
            bare,
            StationCommand::UiResponse(UiResponseCommand { actor: None, .. })
        ));
    }

    #[test]
    fn kiosk_bare_command_parses_as_envelope() {
        let env: CommandEnvelope = serde_json::from_str(r#"{"type":"stop"}"#).unwrap();
        assert!(env.command_id.is_none() && env.actor.is_none());
        assert!(matches!(env.command, StationCommand::Stop { .. }));
    }

    #[test]
    fn actor_never_serialized_inside_ui_response() {
        let cmd = StationCommand::UiResponse(UiResponseCommand {
            request_id: "r".into(),
            values: Default::default(),
            actor: Some(Actor::station()),
        });
        assert!(!serde_json::to_string(&cmd).unwrap().contains("actor"));
    }

    #[test]
    fn accepted_omits_defaults_on_the_wire() {
        let ev = StationEvent::UiResponseAccepted(UiResponseAcceptedPayload::new("r"));
        assert_eq!(
            serde_json::to_string(&ev).unwrap(),
            r#"{"type":"ui_response_accepted","request_id":"r"}"#
        );
    }
}
