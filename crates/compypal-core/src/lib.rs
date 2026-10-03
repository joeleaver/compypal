//! The music model behind compypal: projects, raw recorded sessions, and the
//! cleanup operations that turn one into the other. No audio, no UI, no I/O.

pub mod arrange;
pub mod cleanup;
pub mod demo;
pub mod figure;
pub mod gm;
pub mod history;
pub mod jam;
pub mod model;
pub mod session;
pub mod text;
pub mod theory;

pub use history::History;
pub use model::*;
pub use session::{RawEvent, RawMsg, RawNote, Session};
