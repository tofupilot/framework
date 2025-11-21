use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::execution::reports::ReportManager;

pub async fn collect_attachments(
    report_managers: &Arc<RwLock<HashMap<String, ReportManager>>>,
    job_id: &Uuid,
    slot_id: &Option<String>,
) -> Vec<String> {
    let report_managers_lock = report_managers.read().await;

    if let Some(slot_id) = slot_id {
        if let Some(manager) = report_managers_lock.get(slot_id) {
            manager
                .get_job_attachments(job_id)
                .unwrap_or_else(Vec::new)
        } else {
            Vec::new()
        }
    } else {
        if let Some((_, manager)) = report_managers_lock.iter().next() {
            manager
                .get_job_attachments(job_id)
                .unwrap_or_else(Vec::new)
        } else {
            Vec::new()
        }
    }
}
