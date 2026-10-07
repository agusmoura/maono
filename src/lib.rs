//! Maono PD100W wireless microphone control.
//!
//! The protocol lives here so both frontends - the CLI and the TUI - drive the
//! device through exactly one implementation.

pub mod config;
pub mod descriptor;
pub mod device;
pub mod filter;
pub mod filterctl;
pub mod mic;
pub mod proto;
pub mod profiles;
pub mod pw;
pub mod safety;
pub mod state;
pub mod store;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod testutil;
