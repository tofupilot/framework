//! Job completion handling and result processing

mod attachment_collector;
mod error_handling;
mod event_emitter;
mod handler;
mod next_action;
mod outcome_resolver;

#[cfg(test)]
mod tests;
