//! Explicit direct loopback endpoints. Control and attachment credentials are independent;
//! resolving an endpoint never probes, starts a helper, or authorizes a retry.

use std::net::IpAddr;
use serde::{Deserialize, Serialize};
pub use crate::remote::Descriptor;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Direct { host: String, port: u16, brp: Descriptor },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable(pub String);
impl core::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str(&self.0) }
}
impl core::error::Error for Unavailable {}

#[derive(Debug)]
pub struct Resolved { pub descriptor: Descriptor }

impl Transport {
    pub fn kind(&self) -> &'static str { "direct" }
    pub fn address(&self) -> String {
        let Self::Direct { host, port, .. } = self;
        format!("{host}:{port}")
    }
    pub fn descriptor(&self) -> &Descriptor {
        let Self::Direct { brp, .. } = self;
        brp
    }
    pub fn validate(&self) -> Result<(), String> {
        let Self::Direct { host, port, brp } = self;
        if !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err("direct endpoint must use a literal loopback address".into());
        }
        if *port == 0 { return Err("port 0".into()); }
        if brp.token.is_empty() || brp.instance.is_empty() {
            return Err("descriptor requires a token and instance nonce".into());
        }
        Ok(())
    }
    pub fn resolve(&self) -> Result<Resolved, Unavailable> {
        self.validate().map_err(Unavailable)?;
        let Self::Direct { host, port, brp } = self;
        Ok(Resolved { descriptor: rewrite(brp, host, *port) })
    }
}

pub fn rewrite(brp: &Descriptor, host: &str, port: u16) -> Descriptor {
    let mut descriptor = brp.clone();
    descriptor.http.host = host.to_owned();
    descriptor.http.port = port;
    if let Some(attach) = &mut descriptor.attach { attach.host = host.to_owned(); }
    descriptor
}
