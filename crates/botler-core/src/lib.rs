//! Pure domain logic for botler. No IO lives here: everything is a function of definitions,
//! events and time, so it can be tested exhaustively and reasoned about in isolation.

pub mod catalog;
pub mod change;
pub mod defs;
pub mod duration;
pub mod event;
pub mod event_check;
pub mod limits;
pub mod routing;
pub mod trigger;

pub use defs::{Definitions, Diagnostic, load};
pub use event::{Event, Provenance, Source};
