//! Credentials in the procedure's environment.
//!
//! A run gives its procedure the identity the CLI is logged in with,
//! whether or not it uploads, as three variables set on every process the engine spawns for it:
//! `TOFUPILOT_API_KEY`, `TOFUPILOT_URL` (instance base, no `/api`) and
//! `TOFUPILOT_CA_CERT` (only on a private-CA instance).
//! The host (the CLI) builds the list; the engine only carries it to
//! the spawn sites.
//!
//! A run on a CLI that is not logged in gets none of them. The engine used
//! to pass the CLI's whole environment through, so an ambient
//! `TOFUPILOT_API_KEY` on the station reached phases that never asked for
//! it; the strip below is what makes "no login, no credentials" true.

use std::sync::Once;

/// The variables a logged-in run's procedure receives, and the only
/// `TOFUPILOT_*` names ever removed from a child's inherited environment.
pub const PROCEDURE_ENV_VARS: &[&str] =
    &["TOFUPILOT_API_KEY", "TOFUPILOT_URL", "TOFUPILOT_CA_CERT"];

/// Variables set on every process spawned for one procedure. Empty when
/// the CLI has no credentials.
pub type ProcedureEnv = Vec<(String, String)>;

/// Remove the credential variables a child would otherwise inherit from
/// this process. Called by every spawn builder before the procedure's
/// own env is applied.
pub fn strip_inherited(cmd: &mut tokio::process::Command) {
    warn_once_if_ambient();
    for var in PROCEDURE_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Same as [`strip_inherited`] for a synchronous command.
pub fn strip_inherited_std(cmd: &mut std::process::Command) {
    warn_once_if_ambient();
    for var in PROCEDURE_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Procedures used to inherit an ambient key silently. One warning per
/// process when one is present, so a station that relied on that finds
/// out from its log rather than from a 401 in a phase.
fn warn_once_if_ambient() {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        let ambient: Vec<&str> = PROCEDURE_ENV_VARS
            .iter()
            .copied()
            .filter(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
            .collect();
        if !ambient.is_empty() {
            log::warn!(
                "Ignored {} from the environment: a phase or plug gets the run's credentials \
                 through its `dashboard` parameter",
                ambient.join(", ")
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_only_the_credential_variables() {
        let mut cmd = tokio::process::Command::new("true");
        cmd.env("TOFUPILOT_API_KEY", "k")
            .env("TOFUPILOT_URL", "u")
            .env("TOFUPILOT_CA_CERT", "c")
            .env("TOFUPILOT_LOCAL_UI_PORT", "7321");
        strip_inherited(&mut cmd);
        let std_cmd = cmd.as_std();
        let envs: Vec<(String, Option<String>)> = std_cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for var in PROCEDURE_ENV_VARS {
            assert!(
                envs.iter().any(|(k, v)| k == var && v.is_none()),
                "{var} should be removed"
            );
        }
        assert!(envs
            .iter()
            .any(|(k, v)| k == "TOFUPILOT_LOCAL_UI_PORT" && v.as_deref() == Some("7321")));
    }
}
