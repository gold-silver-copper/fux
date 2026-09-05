#![forbid(unsafe_code)]

pub mod client;
pub mod commands;
pub mod config;
pub mod control;
pub mod daemon;
pub mod host;
pub mod keys;
pub mod pty;
pub mod state;

// Workspace wire schema 2 includes authoritative viewer bindings in replicated metadata.
pub const FUX_ALPN: &[u8] = b"fux/2";

pub fn parse_agent_report(input: &[u8]) -> Result<zor::osc::Report, zor::osc::Error> {
    zor::osc::parse(input)
}
