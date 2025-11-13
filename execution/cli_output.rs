//! CLI output management module
//! Provides structured, clean output for CLI mode

use std::io::{self, IsTerminal};
use colored::Colorize;

/// Initialize the output system
pub fn init() {
    // No-op, keeping for API compatibility
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
}

/// Print a message with a section header
pub fn print_section(section: Section, message: impl AsRef<str>) {
    match section {
        Section::Error => eprintln!("{}", format!("ERROR: {}", message.as_ref()).red()),
        Section::Summary => println!("{}", message.as_ref()),
        _ => println!("[{}] {}", section.as_str(), message.as_ref()),
    }
}

/// Print a message
pub fn print_at_level(message: impl AsRef<str>) {
    println!("{}", message.as_ref());
}

/// Print an indented sub-item
pub fn print_item(message: impl AsRef<str>, indent: usize) {
    let indent_str = "  ".repeat(indent);
    println!("{}• {}", indent_str, message.as_ref());
}

/// Print a status line with alignment and color
pub fn print_status(label: impl AsRef<str>, status: impl AsRef<str>, success: bool) {
    let dots = ".".repeat(50_usize.saturating_sub(label.as_ref().len()));
    let symbol = if success { "PASS".green() } else { "FAIL".red() };
    println!(
        "  {} {} {} {}",
        label.as_ref(),
        dots,
        symbol,
        status.as_ref()
    );
}

/// Print debug information
pub fn debug(message: impl AsRef<str>) {
    eprintln!("    · {}", message.as_ref());
}

/// Print verbose information
pub fn verbose(message: impl AsRef<str>) {
    println!("  → {}", message.as_ref());
}

/// Print an error message
pub fn error(message: impl AsRef<str>) {
    eprintln!("  {}", format!("ERROR: {}", message.as_ref()).red());
}

/// Print application header
pub fn print_header(name: &str, version: &str) {
    println!("{} v{}", name, version);
    println!("{}", "═".repeat(name.len() + version.len() + 2));
    println!();
}

/// Print execution summary
pub fn print_summary(
    passed: bool,
    total_jobs: usize,
    completed: usize,
    failed: usize,
    duration_secs: u64,
) {
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
        if !is_interactive() {
            let status = if success { "PASS" } else { "FAIL" };
            let duration_s = duration_ms as f64 / 1000.0;
            println!("[PHASE]   {} {} {:.1}s", slot_id, status, duration_s);
        }
    }

    pub fn finish(&self) {
        // In non-interactive mode, print phase completion summary
        if !is_interactive() {
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
                println!("  {}", format!("OK: {}", msg).green());
            }
            MessageType::Warning(msg) => {
                println!("  {}", format!("! {}", msg).yellow());
            }
            MessageType::Error(msg) => {
                eprintln!("  {}", format!("ERROR: {}", msg).red());
            }
            MessageType::Info(msg) => {
                println!("  → {}", msg);
            }
            MessageType::Debug(msg) => {
                eprintln!("    · {}", msg);
            }
            MessageType::Progress(msg, current, total) => {
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
    let symbol = if success { "OK" } else { "FAIL" };
    println!("  {}: {} {}", symbol, operation.as_ref(), target.as_ref());
}

/// Print phase transition
pub fn phase_transition(phase_name: impl AsRef<str>, slot_id: Option<&str>, started: bool) {
    let action = if started { "Starting" } else { "Completed" };

    match slot_id {
        Some(slot) => println!("  ● {} phase: {} [{}]", action, phase_name.as_ref(), slot),
        None => println!("  ● {} phase: {}", action, phase_name.as_ref()),
    }
}

/// Print timeout warning
pub fn timeout_warning(phase_name: impl AsRef<str>, slot_id: impl AsRef<str>, timeout_secs: u64) {
    println!(
        "  ! Phase timeout: {} [{}] after {}s",
        phase_name.as_ref(),
        slot_id.as_ref(),
        timeout_secs
    );
}

