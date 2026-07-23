//! OS keyring helpers for integration credentials.

use color_eyre::eyre::{Context, eyre};
use keyring::Entry;

/// Keyring service name for this application.
pub const SERVICE: &str = "tod";

/// Account name for the Linear API key.
pub const LINEAR_ACCOUNT: &str = "linear";

fn linear_entry() -> color_eyre::Result<Entry> {
    Entry::new(SERVICE, LINEAR_ACCOUNT).wrap_err("creating keyring entry for Linear")
}

/// Load the Linear API key from the OS keyring, if present.
pub fn load_linear_api_key() -> color_eyre::Result<Option<String>> {
    let entry = linear_entry()?;
    match entry.get_password() {
        Ok(password) if !password.is_empty() => Ok(Some(password)),
        Ok(_) => Ok(None),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(eyre!("reading Linear API key from keyring: {err}")),
    }
}

/// Store the Linear API key in the OS keyring.
pub fn store_linear_api_key(api_key: &str) -> color_eyre::Result<()> {
    if api_key.trim().is_empty() {
        return Err(eyre!("Linear API key cannot be empty"));
    }
    let entry = linear_entry()?;
    entry
        .set_password(api_key.trim())
        .wrap_err("storing Linear API key in keyring")
}
