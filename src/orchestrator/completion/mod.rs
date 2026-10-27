//! Job completion handling and result processing

pub(in crate::orchestrator) mod error_handling;
mod event_emitter;
mod handler;
mod next_action;
mod outcome_resolver;
mod sequences;

#[cfg(test)]
mod tests;
