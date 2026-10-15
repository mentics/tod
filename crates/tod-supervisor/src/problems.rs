//! The supervisor's own warnings and errors, recorded where the node's
//! problem forwarder finds them (`Replica::forward_problems`): a tracing layer
//! that appends each WARN/ERROR to the node's problem directory
//! (`tod_problems::node_dir`). Nothing here contacts the orchestrator.

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

pub struct ProblemLayer;

#[derive(Default)]
struct Text {
    message: String,
    fields: String,
}

impl Visit for Text {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for ProblemLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => tod_problems::Level::Error,
            Level::WARN => tod_problems::Level::Warn,
            _ => return,
        };
        let mut text = Text::default();
        event.record(&mut text);
        report(level, &format!("{}{}", text.message, text.fields));
    }
}

/// Record a problem of this process (also used for its fatal exit).
pub fn report(level: tod_problems::Level, message: &str) {
    if let Err(err) = tod_problems::report_in(&tod_problems::node_dir(), "tod-supervisor", level, None, message) {
        eprintln!("tod-supervisor: could not record this in the problem log: {err}");
    }
}
