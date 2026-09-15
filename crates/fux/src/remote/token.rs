//! Capability tokens (prompt 3.9): the server token from `brp.json` has every capability over
//! every workspace; `fux/token.mint` derives narrower tokens that are bounded by `Limits.tokens`,
//! never persisted and revocable at once.

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

/// Capabilities are a bit set so a grant compares in one instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Capabilities(u8);

impl Capabilities {
    pub const READ: Self = Self(1);
    pub const MUTATE: Self = Self(2);
    pub const ATTACH: Self = Self(4);
    pub const ADMIN: Self = Self(8);
    pub const ALL: Self = Self(15);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn names(self) -> Vec<Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(|c| self.contains(c.mask()))
            .collect()
    }
}

/// The wire spelling of one capability bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read,
    Mutate,
    Attach,
    Admin,
}

impl Capability {
    pub const ALL: [Self; 4] = [Self::Read, Self::Mutate, Self::Attach, Self::Admin];

    pub const fn mask(self) -> Capabilities {
        match self {
            Self::Read => Capabilities::READ,
            Self::Mutate => Capabilities::MUTATE,
            Self::Attach => Capabilities::ATTACH,
            Self::Admin => Capabilities::ADMIN,
        }
    }
}

/// What a token may do: `workspace == None` means every workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub workspace: Option<String>,
    pub capabilities: Capabilities,
}

impl Grant {
    /// Whether this grant covers the named workspace.
    pub fn covers(&self, workspace: &str) -> bool {
        self.workspace.as_deref().is_none_or(|w| w == workspace)
    }
}

/// Server token plus minted tokens; bounded, never persisted.
#[derive(Resource, Debug)]
pub struct Tokens {
    server: String,
    minted: Vec<(String, Grant)>,
    limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintError {
    /// `Limits.tokens` reached.
    Limit,
}

impl core::fmt::Display for MintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Limit => f.write_str("token limit reached"),
        }
    }
}

impl Tokens {
    pub fn new(server: String, limit: usize) -> Self {
        Self {
            server,
            minted: Vec::new(),
            limit,
        }
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Resolves a presented token to its grant. Every stored token is compared in constant time
    /// and the scan never short-circuits on the first match, so timing reveals nothing about
    /// which entry (if any) matched.
    pub fn authorize(&self, presented: &str) -> Option<Grant> {
        let mut found: Option<Grant> = None;
        if constant_time_eq(self.server.as_bytes(), presented.as_bytes()) {
            found = Some(Grant {
                workspace: None,
                capabilities: Capabilities::ALL,
            });
        }
        for (token, grant) in &self.minted {
            if constant_time_eq(token.as_bytes(), presented.as_bytes()) && found.is_none() {
                found = Some(grant.clone());
            }
        }
        found
    }

    pub fn mint(&mut self, token: String, grant: Grant) -> Result<(), MintError> {
        if self.minted.len() >= self.limit {
            return Err(MintError::Limit);
        }
        self.minted.push((token, grant));
        Ok(())
    }

    /// Removes a minted token; the server token cannot be revoked. Returns whether one matched.
    pub fn revoke(&mut self, presented: &str) -> bool {
        let before = self.minted.len();
        self.minted
            .retain(|(token, _)| !constant_time_eq(token.as_bytes(), presented.as_bytes()));
        self.minted.len() != before
    }

    pub fn minted_count(&self) -> usize {
        self.minted.len()
    }
}

/// Byte-wise fold that touches every byte of both inputs regardless of where they differ.
/// Unequal lengths short-circuit: the length of a hex token is public.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_token_has_every_capability_and_minted_tokens_are_scoped() {
        let mut tokens = Tokens::new("server".into(), 2);
        let all = tokens.authorize("server").unwrap();
        assert_eq!(all.capabilities, Capabilities::ALL);
        assert!(all.covers("anything"));
        tokens
            .mint(
                "narrow".into(),
                Grant {
                    workspace: Some("a".into()),
                    capabilities: Capabilities::READ.union(Capabilities::MUTATE),
                },
            )
            .unwrap();
        let narrow = tokens.authorize("narrow").unwrap();
        assert!(narrow.covers("a"));
        assert!(!narrow.covers("b"));
        assert!(!narrow.capabilities.contains(Capabilities::ADMIN));
        assert!(tokens.authorize("nope").is_none());
        assert!(tokens.authorize("").is_none());
    }

    #[test]
    fn mint_is_bounded_and_revoke_is_immediate() {
        let mut tokens = Tokens::new("server".into(), 1);
        let grant = Grant {
            workspace: None,
            capabilities: Capabilities::READ,
        };
        tokens.mint("one".into(), grant.clone()).unwrap();
        assert_eq!(tokens.mint("two".into(), grant), Err(MintError::Limit));
        assert!(tokens.revoke("one"));
        assert!(!tokens.revoke("one"));
        assert!(!tokens.revoke("server"));
        assert!(tokens.authorize("one").is_none());
        assert!(tokens.authorize("server").is_some());
    }
}
