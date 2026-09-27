//! The pasted relay code (spec §9.1, §9.5): the recipient's public key, the
//! relay server, and the two topics, packed into one string a user can paste.

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "todj1:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayCode {
    pub recipient: String,
    pub server: String,
    pub inbox: String,
    pub ack: String,
}

impl RelayCode {
    /// Encodes as `todj1:` + base64url (no padding) of the CBOR encoding.
    pub fn format(&self) -> Result<String> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(self, &mut bytes).context("failed to encode relay code")?;
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
        Ok(format!("{PREFIX}{encoded}"))
    }

    /// Parses a string produced by [`RelayCode::format`], also validating
    /// that `recipient` is a syntactically valid age X25519 recipient.
    pub fn parse(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| anyhow!("relay code missing '{PREFIX}' prefix"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, rest)
            .context("relay code is not valid base64url")?;
        let code: RelayCode =
            ciborium::de::from_reader(bytes.as_slice()).context("relay code is not valid CBOR")?;

        age::x25519::Recipient::from_str(&code.recipient)
            .map_err(|e| anyhow!("relay code recipient is not a valid age recipient: {e}"))?;

        Ok(code)
    }
}
