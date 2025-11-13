//! CLI output management module
//! Provides structured, clean output for CLI mode with different verbosity levels

use std::io::{self, IsTerminal};
use std::sync::OnceLock;

/// Global output level for CLI
static OUTPUT_LEVEL: OnceLock<OutputLevel> = OnceLock::new();

/// Output verbosity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputLevel {
    /// Minimal output - only errors and final summary
    Quiet = 0,
    /// Default - clean structured output
    Normal = 1,
    /// Include worker assignments and job details
    Verbose = 2,
    /// Everything including IPC and protocol messages
    Debug = 3,
}

impl OutputLevel {
    /// Parse from CLI argument
    pub fn from_cli_arg(quiet: bool, verbose: u8) -> Self {
        if quiet {
            OutputLevel::Quiet
        } else {
            match verbose {
                0 => OutputLevel::Normal,
                1 => OutputLevel::Verbose,
                _ => OutputLevel::Debug,
            }
        }
    }

    /// Check environment variable for log level
    pub fn from_env() -> Option<Self> {
        std::env::var("STUDIO_LOG_LEVEL").ok().and_then(|level| {
            match level.to_lowercase().as_str() {
                "quiet" => Some(OutputLevel::Quiet),
                "normal" => Some(OutputLevel::Normal),
                "verbose" => Some(OutputLevel::Verbose),
                "debug" => Some(OutputLevel::Debug),
                _ => None,
            }
        })
    }
}

/// Initialize the output system with the given level
pub fn init(level: OutputLevel) {
    OUTPUT_LEVEL.set(level).unwrap_or_else(|_| {
        eprintln!("Warning: Output level already initialized");
    });
}

/// Get the current output level
pub fn level() -> OutputLevel {
    *OUTPUT_LEVEL.get().unwrap_or(&OutputLevel::Normal)
}

/// Check if running in a terminal (for progress bars)
pub fn is_interactive() -> bool {
    io::stdout().is_terminal()
}

/// Output sections for structured display
#[derive(Debug, Clone, Copy)]
pub enum Section {
    Init,
    Config,
    Plugs,
    Phase,
    Reports,
    Summary,
    Error,
    Worker,
    System,
}

impl Section {
    fn as_str(&self) -> &'static str {
        match self {
            Section::Init => "[INIT]",
            Section::Config => "[CONFIG]",
            Section::Plugs => "[PLUGS]",
            Section::Phase => "[PHASE]",
            Section::Reports => "[REPORTS]",
            Section::Summary => "[SUMMARY]",
            Section::Error => "[ERROR]",
            Section::Worker => "[WORKER]",
            Section::System => "[SYSTEM]",
        }
    }

    fn min_level(&self) -> OutputLevel {
        match self {
            Section::Error => OutputLevel::Quiet,   // Always show errors
            Section::Summary => OutputLevel::Quiet, // Always show summary
            _ => OutputLevel::Normal,               // Everything else needs Normal+
        }
    }
}

/// Print a message with a section header
pub fn print_section(section: Section, message: impl AsRef<str>) {
    if level() >= section.min_level() {
        println!("[{}] {}", section.as_str(), message.as_ref());
    }
}

/// Print a message at a specific output level
pub fn print_at_level(min_level: OutputLevel, message: impl AsRef<str>) {
    if level() >= min_level {
        println!("{}", message.as_ref());
    }
}

/// Print an indented sub-item (for structured output)
pub fn print_item(message: impl AsRef<str>, indent: usize) {
    if level() >= OutputLevel::Normal {
        let indent_str = "  ".repeat(indent);
        println!("{}• {}", indent_str, message.as_ref());
    }
}

/// Print a status line with alignment
pub fn print_status(label: impl AsRef<str>, status: impl AsRef<str>, success: bool) {
    if level() >= OutputLevel::Normal {
        let dots = ".".repeat(50_usize.saturating_sub(label.as_ref().len()));
        let symbol = if success { "✓" } else { "✗" };
        println!(
            "  {} {} {} {}",
            label.as_ref(),
            dots,
            symbol,
            status.as_ref()
        );
    }
}

/// Print debug information (only in Debug mode)
pub fn debug(message: impl AsRef<str>) {
    if level() >= OutputLevel::Debug {
        eprintln!("    · {}", message.as_ref());
    }
}

/// Print verbose information (only in Verbose mode or higher)
pub fn verbose(message: impl AsRef<str>) {
    if level() >= OutputLevel::Verbose {
        println!("  → {}", message.as_ref());
    }
}

/// Print an error message (always shown)
pub fn error(message: impl AsRef<str>) {
    eprintln!("  ✗ {}", message.as_ref());
}

/// Print a header for the application
pub fn print_header(name: &str, version: &str) {
    if level() >= OutputLevel::Normal {
        println!("{} v{}", name, version);
        println!("{}", "=".repeat(name.len() + version.len() + 2));
        println!();
    }
}

/// Print execution summary
pub fn print_summary(
    passed: bool,
    total_jobs: usize,
    completed: usize,
    failed: usize,
    duration_secs: u64,
) {
    if level() >= OutputLevel::Quiet {
        println!();
        println!("═══════════════════════════════════════════════════════════");
        let status_text = if passed { "PASSED" } else { "FAILED" };
        println!("  Status: {}", status_text);
        println!("  Jobs: {}", total_jobs);
        println!("  Completed: {}", completed);
        println!("  Failed: {}", failed);

        let minutes = duration_secs / 60;
        let seconds = duration_secs % 60;
        println!("  Duration: {}m {}s", minutes, seconds);
        println!("═══════════════════════════════════════════════════════════");
        println!();
    }
}

/// Helper for phase progress display
pub struct PhaseProgress {
    pub phase_name: String,
    pub total_slots: usize,
    pub completed_slots: usize,
    pub slot_results: Vec<(String, bool, u64)>, // (slot_id, success, duration_ms)
}

impl PhaseProgress {
    pub fn new(phase_name: String, total_slots: usize) -> Self {
        Self {
            phase_name,
            total_slots,
            completed_slots: 0,
            slot_results: Vec::new(),
        }
    }

    pub fn add_slot_result(&mut self, slot_id: String, success: bool, duration_ms: u64) {
        self.slot_results
            .push((slot_id.clone(), success, duration_ms));
        self.completed_slots = self.slot_results.len();

        // In non-interactive mode, print each completion
        if !is_interactive() && level() >= OutputLevel::Normal {
            let status = if success { "PASS" } else { "FAIL" };
            let duration_s = duration_ms as f64 / 1000.0;
            println!("[PHASE]   {} {} {:.1}s", slot_id, status, duration_s);
        }
    }

    pub fn finish(&self) {
        // In non-interactive mode, print phase completion summary
        if !is_interactive() && level() >= OutputLevel::Normal {
            let passed = self
                .slot_results
                .iter()
                .filter(|(_, success, _)| *success)
                .count();
            let failed = self.completed_slots - passed;
            println!(
                "[PHASE] {} completed ({}/{} slots, {} PASS, {} FAIL)",
                self.phase_name, self.completed_slots, self.total_slots, passed, failed
            );
        }
    }
}

/// Unified message types for consistent formatting
#[derive(Debug, Clone)]
pub enum MessageType {
    Success(String),
    Warning(String),
    Error(String),
    Info(String),
    Debug(String),
    Progress(String, usize, usize), // message, current, total
}

impl MessageType {
    pub fn print(&self) {
        match self {
            MessageType::Success(msg) => {
                if level() >= OutputLevel::Normal {
                    println!("  ✓ {}", msg);
                }
            }
            MessageType::Warning(msg) => {
                if level() >= OutputLevel::Normal {
                    println!("  ! {}", msg);
                }
            }
            MessageType::Error(msg) => {
                eprintln!("  ✗ {}", msg);
            }
            MessageType::Info(msg) => {
                if level() >= OutputLevel::Verbose {
                    println!("  → {}", msg);
                }
            }
            MessageType::Debug(msg) => {
                if level() >= OutputLevel::Debug {
                    eprintln!("    · {}", msg);
                }
            }
            MessageType::Progress(msg, current, total) => {
                if level() >= OutputLevel::Normal {
                    let percentage = if *total > 0 {
                        (*current * 100) / total
                    } else {
                        0
                    };
                    println!("  → {} [{}/{}] ({}%)", msg, current, total, percentage);
                }
            }
        }
    }
}

/// Convenient functions for unified messaging
pub fn success(message: impl AsRef<str>) {
    MessageType::Success(message.as_ref().to_string()).print();
}

pub fn warning(message: impl AsRef<str>) {
    MessageType::Warning(message.as_ref().to_string()).print();
}

pub fn info(message: impl AsRef<str>) {
    MessageType::Info(message.as_ref().to_string()).print();
}

pub fn progress_msg(message: impl AsRef<str>, current: usize, total: usize) {
    MessageType::Progress(message.as_ref().to_string(), current, total).print();
}

/// Print system operation (plug creation, worker spawn, etc.)
pub fn system_operation(operation: impl AsRef<str>, target: impl AsRef<str>, success: bool) {
    if level() >= OutputLevel::Normal {
        let symbol = if success { "✓" } else { "✗" };
        println!("  {} {} {}", symbol, operation.as_ref(), target.as_ref());
    }
}

/// Print phase transition
pub fn phase_transition(phase_name: impl AsRef<str>, slot_id: Option<&str>, started: bool) {
    if level() >= OutputLevel::Normal {
        let action = if started { "Starting" } else { "Completed" };

        match slot_id {
            Some(slot) => println!("  ● {} phase: {} [{}]", action, phase_name.as_ref(), slot),
            None => println!("  ● {} phase: {}", action, phase_name.as_ref()),
        }
    }
}

/// Print timeout warning
pub fn timeout_warning(phase_name: impl AsRef<str>, slot_id: impl AsRef<str>, timeout_secs: u64) {
    if level() >= OutputLevel::Normal {
        println!(
            "  ! Phase timeout: {} [{}] after {}s",
            phase_name.as_ref(),
            slot_id.as_ref(),
            timeout_secs
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_levels() {
        assert!(OutputLevel::Quiet < OutputLevel::Normal);
        assert!(OutputLevel::Normal < OutputLevel::Verbose);
        assert!(OutputLevel::Verbose < OutputLevel::Debug);
    }

    #[test]
    fn test_from_cli_arg() {
        assert_eq!(OutputLevel::from_cli_arg(true, 0), OutputLevel::Quiet);
        assert_eq!(OutputLevel::from_cli_arg(false, 0), OutputLevel::Normal);
        assert_eq!(OutputLevel::from_cli_arg(false, 1), OutputLevel::Verbose);
        assert_eq!(OutputLevel::from_cli_arg(false, 2), OutputLevel::Debug);
    }
}
