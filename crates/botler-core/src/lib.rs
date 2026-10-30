//! Pure domain logic for botler. No IO lives here: everything is a function of definitions,
//! events and time, so it can be tested exhaustively and reasoned about in isolation.

pub mod catalog;
pub mod change;
pub mod clock;
pub mod defs;
pub mod duration;
pub mod event;
pub mod event_check;
pub mod limits;
pub mod loops;
pub mod routing;
pub mod trigger;

pub use defs::{Definitions, Diagnostic, load, load_with};
pub use event::{Event, Provenance, Source};

/// The always-on chat the user talks to by default (instance and definition name): it receives
/// alarms, breaker notices and failures nobody else handles. Not called `main`, which agents
/// already use for the monorepo's main branch.
pub const FRONT_DOOR: &str = "botler";
