use crate::execution::reports::{DashboardInfo, Report, ReportSummary};
use std::fs;
use std::path::{Path, PathBuf};
use mime_guess;

fn get_report_path(procedure_dir: &str, run_dir: &str) -> PathBuf {
    Path::new(procedure_dir).join("reports").join(run_dir)
}

fn get_report_file_path(procedure_dir: &str, run_dir: &str) -> PathBuf {
    get_report_path(procedure_dir, run_dir).join("report.json")
}

fn update_report_dashboard<F>(
    procedure_dir: String,
    run_dir: String,
    updater: F,
) -> Result<(), String>
where
    F: FnOnce(&mut Report),
{
    let report_file = get_report_file_path(&procedure_dir, &run_dir);

    if !report_file.exists() {
        return Err(format!("Report file not found: {:?}", report_file));
    }

    let content =
        fs::read_to_string(&report_file).map_err(|e| format!("Failed to read report: {}", e))?;

    let mut report: Report =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse report: {}", e))?;

    updater(&mut report);

    let updated_json = serde_json::to_string_pretty(&report)
        .map_err(|e| format!("Failed to serialize report: {}", e))?;

    fs::write(&report_file, updated_json)
        .map_err(|e| format!("Failed to write report: {}", e))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_report(procedure_dir: String, run_dir: String) -> Result<String, String> {
    let report_file = get_report_file_path(&procedure_dir, &run_dir);
    fs::read_to_string(report_file).map_err(|e| format!("Failed to read report: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn mark_report_uploaded(
    procedure_dir: String,
    run_dir: String,
    dashboard_run_id: String,
) -> Result<(), String> {
    update_report_dashboard(procedure_dir, run_dir, |report| {
        report.dashboard = Some(DashboardInfo {
            run_id: Some(dashboard_run_id),
            upload_error: None,
        });
    })
}

#[tauri::command]
#[specta::specta]
pub async fn mark_report_upload_failed(
    procedure_dir: String,
    run_dir: String,
    error: String,
) -> Result<(), String> {
    update_report_dashboard(procedure_dir, run_dir, |report| {
        let existing_run_id = report.dashboard.as_ref().and_then(|d| d.run_id.clone());
        report.dashboard = Some(DashboardInfo {
            run_id: existing_run_id,
            upload_error: Some(error),
        });
    })
}

#[tauri::command]
#[specta::specta]
pub async fn list_report_attachments(
    procedure_dir: String,
    run_dir: String,
) -> Result<String, String> {
    let run_path = get_report_path(&procedure_dir, &run_dir);

    if !run_path.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    let entries =
        fs::read_dir(&run_path).map_err(|e| format!("Failed to read report directory: {}", e))?;

    let mut attachments = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if path.is_file() {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            if file_name != "report.json" {
                let metadata = fs::metadata(&path)
                    .map_err(|e| format!("Failed to get file metadata: {}", e))?;

                let mime_type = mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string();

                let full_path = path.to_string_lossy().to_string();

                attachments.push(serde_json::json!({
                    "name": file_name,
                    "path": full_path,
                    "size": metadata.len(),
                    "mime_type": mime_type,
                }));
            }
        }
    }

    let result = serde_json::json!(attachments);
    serde_json::to_string(&result).map_err(|e| format!("Failed to serialize: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn list_reports(procedure_dir: String) -> Result<String, String> {
    let reports_dir = Path::new(&procedure_dir).join("reports");

    if !reports_dir.exists() {
        return Ok(r#"{"runs":[]}"#.to_string());
    }

    let mut summaries: Vec<ReportSummary> = Vec::new();

    let entries = fs::read_dir(&reports_dir)
        .map_err(|e| format!("Failed to read reports directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let run_file = path.join("report.json");
        if !run_file.exists() {
            continue;
        }

        let content = match fs::read_to_string(&run_file) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[get_test_runs] Failed to read {:?}: {}", run_file, e);
                continue;
            }
        };

        let report: Report = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("[get_test_runs] Failed to parse {:?}: {}", run_file, e);
                continue;
            }
        };

        let directory = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let failed_phases = report.phases
            .iter()
            .filter(|p| matches!(p.outcome, crate::execution::job::Outcome::Fail | crate::execution::job::Outcome::Error))
            .count();

        summaries.push(ReportSummary {
            run_id: report.run_id,
            execution_id: report.execution_id,
            timestamp: report.timestamp,
            directory,
            outcome: format!("{:?}", report.outcome).to_lowercase(),
            duration_ms: report.duration_ms,
            total_phases: report.phases.len(),
            failed_phases,
            unit: report.unit,
            has_attachments: report.phases.iter().any(|p| !p.attachments.is_empty()),
            dashboard: report.dashboard,
        });
    }

    summaries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    log::debug!(
        "[get_test_runs] Returning {} runs from {}",
        summaries.len(),
        procedure_dir
    );

    let result = serde_json::json!({ "runs": summaries });
    serde_json::to_string(&result).map_err(|e| format!("Failed to serialize: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn read_attachment_file(file_path: String) -> Result<Vec<u8>, String> {
    let path = Path::new(&file_path);

    if !path.exists() {
        return Err(format!("File not found: {}", file_path));
    }

    if !path.is_file() {
        return Err(format!("Path is not a file: {}", file_path));
    }

    fs::read(path).map_err(|e| format!("Failed to read file: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn delete_report(procedure_dir: String, run_dir: String) -> Result<(), String> {
    let report_path = get_report_path(&procedure_dir, &run_dir);

    if !report_path.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    fs::remove_dir_all(&report_path)
        .map_err(|e| format!("Failed to delete report directory: {}", e))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn open_report_dir(procedure_dir: String, run_dir: String) -> Result<(), String> {
    let report_path = get_report_path(&procedure_dir, &run_dir);

    if !report_path.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&report_path)
            .spawn()
            .map_err(|e| format!("Failed to open directory: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&report_path)
            .spawn()
            .map_err(|e| format!("Failed to open directory: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&report_path)
            .spawn()
            .map_err(|e| format!("Failed to open directory: {}", e))?;
    }

    Ok(())
}
