//! Sealing bundles for delivery: `age` (X25519) encryption, plus splitting a
//! blob into parts under a relay's message-size limit.

use std::io::{Read, Write};
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};

/// Encrypts `plaintext` so only the holder of `recipient`'s matching identity
/// can read it.
pub fn seal(recipient: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let recipient = age::x25519::Recipient::from_str(recipient)
        .map_err(|e| anyhow!("invalid age recipient: {e}"))?;
    let recipients: Vec<&dyn age::Recipient> = vec![&recipient];
    let encryptor = age::Encryptor::with_recipients(recipients.into_iter())
        .map_err(|e| anyhow!("failed to build age encryptor: {e}"))?;

    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .context("failed to start age encryption")?;
    writer
        .write_all(plaintext)
        .context("failed to write plaintext")?;
    writer.finish().context("failed to finish age encryption")?;
    Ok(out)
}

/// Decrypts a bundle sealed with [`seal`], given the matching identity
/// (`AGE-SECRET-KEY-1...`).
pub fn open(identity: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let identity = age::x25519::Identity::from_str(identity)
        .map_err(|e| anyhow!("invalid age identity: {e}"))?;
    let decryptor = age::Decryptor::new(ciphertext).context("failed to parse age ciphertext")?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .context("failed to decrypt")?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .context("failed to read decrypted plaintext")?;
    Ok(out)
}

/// Splits `bytes` into chunks of at most `max` bytes each.
pub fn split(bytes: &[u8], max: usize) -> Vec<Vec<u8>> {
    if max == 0 {
        return vec![bytes.to_vec()];
    }
    if bytes.is_empty() {
        return vec![Vec::new()];
    }
    bytes.chunks(max).map(|c| c.to_vec()).collect()
}

/// Reassembles parts produced by [`split`].
pub fn join(parts: Vec<Vec<u8>>) -> Vec<u8> {
    parts.into_iter().flatten().collect()
}

/// Generates a fresh (identity, recipient) pair for tests that need to seal
/// and open a bundle end-to-end without a real relay code. Not for
/// production use — callers outside this crate only reach it from `#[cfg(test)]`.
pub fn generate_test_identity() -> (String, String) {
    use age::secrecy::ExposeSecret;
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();
    (identity.to_string().expose_secret().to_string(), recipient)
}
