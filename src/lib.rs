#![forbid(unsafe_code)]

pub mod client;
pub mod commands;
pub mod config;
pub mod control;
pub mod daemon;
pub mod host;
pub mod local;
pub mod observation;
pub mod pty;
pub mod state;
pub mod terminal;

pub fn parse_agent_report(
    input: &[u8],
) -> Result<crate::observation::Report, crate::observation::Error> {
    crate::observation::parse(input)
}
