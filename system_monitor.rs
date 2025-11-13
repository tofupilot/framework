use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sysinfo::System;
use tauri::State;
use tokio::sync::RwLock;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SystemStats {
    pub timestamp: i64,
    pub cpu_usage: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub memory_percentage: f32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MetricsHistory {
    pub timestamps: Vec<i64>,
    pub cpu_usage: Vec<f32>,
    pub memory_usage: Vec<f32>,
    pub duration_ms: u64,
}

pub struct SystemMonitor {
    system: Arc<Mutex<System>>,
    metrics_history: Arc<RwLock<HashMap<String, Vec<SystemStats>>>>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();

        Self {
            system: Arc::new(Mutex::new(system)),
            metrics_history: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_stats(&self) -> SystemStats {
        let mut system = self.system.lock()
            .expect("system monitor mutex poisoned");

        system.refresh_cpu_all();
        system.refresh_memory();

        let cpu_usage = system.global_cpu_usage();
        let memory_used = system.used_memory();
        let memory_total = system.total_memory();
        let memory_percentage = if memory_total > 0 {
            (memory_used as f32 / memory_total as f32) * 100.0
        } else {
            0.0
        };

        SystemStats {
            timestamp: chrono::Utc::now().timestamp_millis(),
            cpu_usage,
            memory_used,
            memory_total,
            memory_percentage,
        }
    }

    pub async fn get_metrics_history(&self, execution_id: &str) -> Option<MetricsHistory> {
        let history = self.metrics_history.read().await;

        if let Some(metrics) = history.get(execution_id) {
            if metrics.is_empty() {
                return None;
            }

            let start_time = metrics.first()
                .expect("metrics is not empty, checked above").timestamp;
            let end_time = metrics.last()
                .expect("metrics is not empty, checked above").timestamp;
            let duration_ms = (end_time - start_time) as u64;

            Some(MetricsHistory {
                timestamps: metrics.iter().map(|m| m.timestamp - start_time).collect(),
                cpu_usage: metrics.iter().map(|m| m.cpu_usage).collect(),
                memory_usage: metrics.iter().map(|m| m.memory_percentage).collect(),
                duration_ms,
            })
        } else {
            None
        }
    }

    pub async fn clear_history(&self, execution_id: &str) {
        let mut history = self.metrics_history.write().await;
        history.remove(execution_id);
    }
}

#[tauri::command]
pub async fn start_monitoring(
    execution_id: String,
    monitor: State<'_, Arc<SystemMonitor>>,
) -> Result<(), String> {
    let system_monitor = monitor.inner().clone();
    let exec_id = execution_id.clone();

    // Clear any existing history for this execution
    system_monitor.clear_history(&exec_id).await;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500)); // Sample every 500ms
        let mut should_continue = true;

        while should_continue {
            interval.tick().await;
            let stats = system_monitor.get_stats();

            // Store the stats in history
            let mut history = system_monitor.metrics_history.write().await;
            history
                .entry(exec_id.clone())
                .or_insert_with(Vec::new)
                .push(stats);

            // Stop if we've been monitoring for too long (safety limit of 1 hour)
            if let Some(metrics) = history.get(&exec_id) {
                if metrics.len() > 7200 {
                    // 3600 seconds * 2 samples per second
                    should_continue = false;
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn stop_monitoring(
    execution_id: String,
    monitor: State<'_, Arc<SystemMonitor>>,
) -> Result<MetricsHistory, String> {
    // Get the final metrics
    if let Some(history) = monitor.get_metrics_history(&execution_id).await {
        Ok(history)
    } else {
        Err("No metrics found for this execution".to_string())
    }
}

#[tauri::command]
pub async fn get_metrics_history(
    execution_id: String,
    monitor: State<'_, Arc<SystemMonitor>>,
) -> Result<MetricsHistory, String> {
    monitor
        .get_metrics_history(&execution_id)
        .await
        .ok_or_else(|| "No metrics found for this execution".to_string())
}
