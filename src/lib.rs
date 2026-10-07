//! Maono PD100W wireless microphone control.
//!
//! The protocol lives here so both frontends - the CLI and the TUI - drive the
//! device through exactly one implementation.

pub mod descriptor;
pub mod device;
pub mod mic;
pub mod proto;
pub mod safety;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod testutil;
