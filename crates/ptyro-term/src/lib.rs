//! Terminal emulation for ptyro (SPEC §5): make a program believe it runs in a good terminal, and
//! expose that terminal's state. Knows nothing about agents, projects or the network.
//!
//! - [`emulator`]: the `Emulator` trait, the alacritty backend and the query responder.
//! - [`frame`]: row versions and screen-frame building.
//! - [`input`]: abstract input → bytes, against the child's modes.
//! - [`sniff`]: the side-channel scanner (OSC 7501, notifications, queries alacritty can't answer).

pub mod emulator;
pub mod frame;
pub mod input;
pub mod modes;
pub mod record;
pub mod sniff;

pub use emulator::{AlacrittyEmulator, Emulator, FeedOutput, TermEvent};
pub use frame::{FrameTracker, StyleTable};
pub use modes::TermModes;
pub use sniff::PspReport;
