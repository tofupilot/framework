//! Every `code` a procedure diagnostic carries, in one place. Tools
//! branch on these (`tofupilot validate --json`, Studio's Issues panel):
//! never rename a shipped one, and add a constant here rather than write a
//! literal at the site, so a typo cannot mint a new code.

// The file as a whole.
/// Not YAML at all: nothing else was judged.
pub const PARSE_ERROR: &str = "parse_error";
/// A required field is absent (`name`, a phase's `key`).
pub const MISSING_FIELD: &str = "missing_field";
/// A field the schema does not have (`validatorz`).
pub const UNKNOWN_FIELD: &str = "unknown_field";
/// A value outside the field's choices (`scope: global`).
pub const UNKNOWN_VALUE: &str = "unknown_value";
/// Two phases, plugs or slots with one key.
pub const DUPLICATE_KEY: &str = "duplicate_key";
/// `depends_on` names no phase of the procedure.
pub const UNKNOWN_DEPENDENCY: &str = "unknown_dependency";
/// Any other rule of the procedure schema.
pub const INVALID_PROCEDURE: &str = "invalid_procedure";
/// Loads today, refused by a future version.
pub const DEPRECATED: &str = "deprecated";
/// A `{namespace.field}` whose namespace is misspelt: kept as text.
pub const UNKNOWN_MARKER: &str = "unknown_marker";
/// A marker where markers do not resolve: kept as text.
pub const MARKER_AS_TEXT: &str = "marker_as_text";
pub const BARE_DURATION: &str = "bare_duration";
/// A secret passed through `env:`, which every child process inherits.
pub const SECRET_IN_ENV: &str = "secret_in_env";
/// Python source that does not compile.
pub const PYTHON_SYNTAX: &str = "python_syntax";
/// Could not be judged here, for want of the procedure's Python: a
/// `uses: <package>:<path>` without its environment, Python sources no
/// interpreter could compile (`tofupilot validate`).
pub const NOT_CHECKED: &str = "not_checked";

// References from the procedure to its Python code.
pub const PHASE_PYTHON_INVALID: &str = "phase_python_invalid";
pub const PHASE_FILE_NOT_FOUND: &str = "phase_file_not_found";
pub const PHASE_CALLABLE_NOT_FOUND: &str = "phase_callable_not_found";
pub const PHASE_MODULE_NOT_IN_PROJECT: &str = "phase_module_not_in_project";
pub const PHASE_MISSING_PARAM: &str = "phase_missing_param";
pub const PHASE_UNKNOWN_PARAM: &str = "phase_unknown_param";
pub const PHASE_PARAM_RENAMED: &str = "phase_param_renamed";
pub const PHASE_RESULT_NOT_RECEIVED: &str = "phase_result_not_received";
pub const PHASE_PLUG_OUT_OF_SCOPE: &str = "phase_plug_out_of_scope";
pub const PHASE_SCOPE_IGNORED: &str = "phase_scope_ignored";
pub const PHASE_PLUG_METHOD_UNKNOWN: &str = "phase_plug_method_unknown";
/// A plug method called with a keyword argument: the phase-side proxy
/// takes positional arguments only.
pub const PHASE_PLUG_CALL_KEYWORD: &str = "phase_plug_call_keyword";
/// `await` on a plug call: the proxy returns the value, not a coroutine.
pub const PHASE_PLUG_CALL_AWAITED: &str = "phase_plug_call_awaited";
/// A plug property called like a method: the proxy refuses it.
pub const PHASE_PLUG_PROPERTY_CALLED: &str = "phase_plug_property_called";
pub const PYTHON_IMPORT_MISSING: &str = "python_import_missing";
pub const PLUG_PYTHON_INVALID: &str = "plug_python_invalid";
pub const PLUG_CLASS_NOT_FOUND: &str = "plug_class_not_found";
pub const PLUG_MISSING_PARAM: &str = "plug_missing_param";
pub const PLUG_UNKNOWN_PARAM: &str = "plug_unknown_param";
pub const SEQUENCE_OUTSIDE_ROOT: &str = "sequence_outside_root";
pub const SEQUENCE_PLUG_CLASS_MISMATCH: &str = "sequence_plug_class_mismatch";
pub const EXECUTABLE_COMMAND_EMPTY: &str = "executable_command_empty";

// Measurements and the operator UI.
pub const DUPLICATE_MEASUREMENT_KEY: &str = "duplicate_measurement_key";
pub const MEASUREMENT_AXIS_OPERATOR: &str = "measurement_axis_operator";
pub const MEASUREMENT_NOT_RECORDED: &str = "measurement_not_recorded";
pub const MEASUREMENT_LIMITS_CONTRADICT: &str = "measurement_limits_contradict";
pub const UNIT_SERIAL_SHARED: &str = "unit_serial_shared";
pub const UI_PANEL: &str = "ui_panel";
pub const UI_DUPLICATE_KEY: &str = "ui_duplicate_key";
pub const UI_INEFFECTIVE_PROPERTY: &str = "ui_ineffective_property";
pub const UI_MEASUREMENT_TYPE_MISMATCH: &str = "ui_measurement_type_mismatch";
pub const UI_OPTIONS_UNFILLED: &str = "ui_options_unfilled";
pub const UI_BIND_UNDECLARED: &str = "ui_bind_undeclared";
