use crate::execution::job::ResourceMetrics;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Tracks resource usage (CPU, memory) for any process by PID
/// Works for Python, Shell, and other external process types
#[derive(Debug)]
pub struct ResourceTracker {
    system: Arc<Mutex<System>>,
    pid: Option<u32>,
    start_time: Option<Instant>,
}

impl ResourceTracker {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_all();

        Self {
            system: Arc::new(Mutex::new(system)),
            pid: None,
            start_time: None,
        }
    }

    /// Start tracking resources for a process with the given PID
    /// If pid is None, only execution time will be tracked
    pub fn start_tracking(&mut self, pid: Option<u32>) {
        self.pid = pid;
        self.start_time = Some(Instant::now());

        // Initialize CPU tracking by refreshing the process
        if let Some(pid) = pid {
            let mut system = self.system.lock().unwrap();
            let sysinfo_pid = Pid::from(pid as usize);
            system.refresh_processes(ProcessesToUpdate::Some(&[sysinfo_pid]), true);

            crate::execution::cli_output::debug(format!(
                "Started resource tracking for PID: {} (converted to {:?})",
                pid, sysinfo_pid
            ));
        } else {
            crate::execution::cli_output::debug(
                "Started resource tracking (time only, no PID available)"
            );
        }
    }

    /// Collect resource metrics for the tracked process
    /// Returns None if no PID was tracked (native phases) or process not found
    pub fn collect_metrics(&mut self) -> Option<ResourceMetrics> {
        let execution_time = self.start_time
            .map(|start| start.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        // If no PID, return minimal metrics with just execution time
        let pid = match self.pid {
            Some(p) => p,
            None => {
                crate::execution::cli_output::debug(
                    "No PID available for resource tracking - returning minimal metrics"
                );
                return Some(ResourceMetrics {
                    cpu_usage_percent: 0.0,
                    cpu_time_seconds: execution_time,
                    memory_peak_mb: 0.0,
                    memory_avg_mb: 0.0,
                    process_count: 0,
                });
            }
        };

        let sysinfo_pid = Pid::from(pid as usize);

        crate::execution::cli_output::debug(format!(
            "Collecting resource metrics for PID: {} (converted to {:?})",
            pid, sysinfo_pid
        ));

        let mut system = self.system.lock().unwrap();

        // Refresh process info
        system.refresh_processes(ProcessesToUpdate::Some(&[sysinfo_pid]), false);

        // Get the main process
        let process = match system.process(sysinfo_pid) {
            Some(p) => p,
            None => {
                crate::execution::cli_output::warning(format!(
                    "Process with PID {} not found in system - it may have already terminated",
                    pid
                ));
                return None;
            }
        };

        // Collect metrics from main process and all children
        let mut total_cpu_usage = process.cpu_usage();
        let mut total_memory = process.memory();
        let mut _total_virtual_memory = process.virtual_memory();
        let mut process_count = 1;

        // Function to recursively get all descendants
        fn get_all_descendants(system: &System, parent_pid: Pid, descendants: &mut Vec<Pid>) {
            for (pid, process) in system.processes() {
                if process.parent() == Some(parent_pid) {
                    descendants.push(*pid);
                    get_all_descendants(system, *pid, descendants);
                }
            }
        }

        // Get all child processes recursively
        let mut child_pids = Vec::new();
        get_all_descendants(&system, sysinfo_pid, &mut child_pids);

        // Add child process metrics
        for child_pid in child_pids {
            if let Some(child) = system.process(child_pid) {
                total_cpu_usage += child.cpu_usage();
                total_memory += child.memory();
                _total_virtual_memory += child.virtual_memory();
                process_count += 1;
            }
        }

        // Calculate CPU time in seconds (cumulative)
        let cpu_time_seconds = (process.run_time() as f64) * (total_cpu_usage as f64 / 100.0);

        let metrics = ResourceMetrics {
            cpu_usage_percent: total_cpu_usage,
            cpu_time_seconds,
            memory_peak_mb: (total_memory as f64) / 1024.0 / 1024.0,
            memory_avg_mb: (total_memory as f64) / 1024.0 / 1024.0, // For now, same as peak
            process_count,
        };

        crate::execution::cli_output::debug(format!(
            "Collected metrics - CPU: {:.1}%, Memory: {:.1}MB, Processes: {}",
            metrics.cpu_usage_percent, metrics.memory_peak_mb, metrics.process_count
        ));

        Some(metrics)
    }
}

impl Default for ResourceTracker {
    fn default() -> Self {
        Self::new()
    }
}
