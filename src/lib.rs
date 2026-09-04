#![forbid(unsafe_code)]

pub mod client;
pub mod config;
pub mod control;
pub mod daemon;
pub mod host;
pub mod state;

pub const FUX_ALPN: &[u8] = b"fux/1";

pub fn parse_agent_report(input: &[u8]) -> Result<zor::osc::Report, zor::osc::Error> {
    zor::osc::parse(input)
}
