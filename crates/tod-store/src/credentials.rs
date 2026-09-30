//! Secure credential storage with OS keyring first, encrypted file fallback.

use crate::paths::TodPaths;
use anyhow::Result;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

const KEYRING_SERVICE: &str = "tod";
const FILE_MAGIC: &[u8; 7] = b"TODENC1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    LinearApiKey,
    GithubToken,
    /// Creates and deletes cloud sandboxes (`tod-sandbox`). Deliberately not in
    /// [`CredentialKind::ALL`], so no agent can read it through `tod-cli secrets`.
    BlaxelApiKey,
    /// The user's Claude subscription token (`claude setup-token`), for the
    /// Claude Code agents in autonomous nodes' cloud sandboxes. Not in
    /// [`CredentialKind::ALL`]: no agent reads it through `tod-cli secrets`.
    ClaudeOauthToken,
}

impl CredentialKind {
    /// Every kind an agent may use, in the order `tod-cli secrets list` shows them.
    pub const ALL: [Self; 2] = [Self::LinearApiKey, Self::GithubToken];

    /// The name an agent uses for this secret (`tod-cli secrets`).
    pub fn name(self) -> &'static str {
        self.keyring_account()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LinearApiKey => "Linear API key",
            Self::GithubToken => "GitHub token",
            Self::BlaxelApiKey => "Blaxel API key",
            Self::ClaudeOauthToken => "Claude subscription token",
        }
    }

    fn keyring_account(self) -> &'static str {
        match self {
            Self::LinearApiKey => "linear_api_key",
            Self::GithubToken => "github_token",
            Self::BlaxelApiKey => "blaxel_api_key",
            Self::ClaudeOauthToken => "claude_oauth_token",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::LinearApiKey => "linear_api_key.enc",
            Self::GithubToken => "github_token.enc",
            Self::BlaxelApiKey => "blaxel_api_key.enc",
            Self::ClaudeOauthToken => "claude_oauth_token.enc",
        }
    }

    fn env_var(self) -> Option<&'static str> {
        match self {
            Self::LinearApiKey => Some("LINEAR_API_KEY"),
            Self::GithubToken => Some("GITHUB_TOKEN"),
            Self::BlaxelApiKey => Some("BL_API_KEY"),
            // What Claude Code itself reads.
            Self::ClaudeOauthToken => Some("CLAUDE_CODE_OAUTH_TOKEN"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialBackend {
    Keyring,
    EncryptedFile,
    Environment,
    /// Not held here at all: an autonomous node's sandbox, whose proxy adds
    /// the user's GitHub token to every request for GitHub
    /// ([`crate::github::GITHUB_AUTH_ENV`]).
    Proxy,
}

/// What stands in for the GitHub token where the proxy injects it: `gh` and
/// `tod-cli secrets run` get this, and the proxy replaces the header they
/// send (`tod_sandbox::node::GH_TOKEN_PLACEHOLDER`, the sandbox's `GH_TOKEN`).
pub const GITHUB_PROXY_PLACEHOLDER: &str = tod_sandbox::node::GH_TOKEN_PLACEHOLDER;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential not found")]
    NotFound,
    #[error("{0}")]
    Message(String),
}

impl From<anyhow::Error> for CredentialError {
    fn from(err: anyhow::Error) -> Self {
        Self::Message(err.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct CredentialStore {
    credentials_dir: PathBuf,
    /// OS keyring service name. Always `KEYRING_SERVICE` outside tests; tests use a
    /// unique name so they never touch (or delete) the user's real stored credentials.
    keyring_service: String,
    /// GitHub is authenticated by the sandbox's proxy
    /// ([`crate::github::proxy_authenticated`], read when the store is made).
    github_via_proxy: bool,
}

impl CredentialStore {
    pub fn new(paths: &TodPaths) -> Self {
        Self {
            credentials_dir: paths.credentials_dir(),
            keyring_service: KEYRING_SERVICE.to_string(),
            github_via_proxy: crate::github::proxy_authenticated(),
        }
    }

    pub fn from_data_root(data_root: &Path) -> Self {
        Self {
            credentials_dir: data_root.join("credentials"),
            keyring_service: KEYRING_SERVICE.to_string(),
            github_via_proxy: crate::github::proxy_authenticated(),
        }
    }

    /// This store as if GitHub were (or were not) authenticated by the
    /// sandbox's proxy, whatever the environment says.
    pub fn with_github_via_proxy(mut self, via_proxy: bool) -> Self {
        self.github_via_proxy = via_proxy;
        self
    }

    fn proxied(&self, kind: CredentialKind) -> bool {
        kind == CredentialKind::GithubToken && self.github_via_proxy
    }

    /// Read a credential using the most secure available source. Where the
    /// sandbox's proxy injects the GitHub token and none is stored, the
    /// GitHub token is [`GITHUB_PROXY_PLACEHOLDER`]: a command given it
    /// (`tod-cli secrets run`) sends it through the proxy, which replaces it.
    pub fn get(&self, kind: CredentialKind) -> Option<String> {
        match self.get_from_keyring(kind.keyring_account()) {
            Ok(Some(value)) => return Some(value),
            Ok(None) | Err(_) => {}
        }
        if let Ok(value) = self.get_from_file(kind.file_name()) {
            return Some(value);
        }
        if self.proxied(kind) {
            return Some(GITHUB_PROXY_PLACEHOLDER.to_string());
        }
        kind.env_var()
            .and_then(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub fn backend(&self, kind: CredentialKind) -> Option<CredentialBackend> {
        if self.get_from_keyring(kind.keyring_account()).ok().flatten().is_some() {
            return Some(CredentialBackend::Keyring);
        }
        if self.file_path(kind.file_name()).is_file() {
            return Some(CredentialBackend::EncryptedFile);
        }
        if self.proxied(kind) {
            return Some(CredentialBackend::Proxy);
        }
        if kind
            .env_var()
            .and_then(|name| std::env::var(name).ok())
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Some(CredentialBackend::Environment);
        }
        None
    }

    /// Persist a credential using the most secure available backend.
    pub fn set(
        &self,
        kind: CredentialKind,
        secret: &str,
    ) -> Result<CredentialBackend, CredentialError> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(CredentialError::Message(
                "credential cannot be empty".into(),
            ));
        }

        if self.set_in_keyring(kind.keyring_account(), secret).is_ok()
            && self
                .get_from_keyring(kind.keyring_account())
                .ok()
                .flatten()
                .is_some_and(|stored| stored == secret)
        {
            let _ = self.remove_file(kind.file_name());
            return Ok(CredentialBackend::Keyring);
        }

        self.set_in_file(kind.file_name(), secret)?;
        Ok(CredentialBackend::EncryptedFile)
    }

    pub fn delete(&self, kind: CredentialKind) -> Result<(), CredentialError> {
        let _ = self.delete_from_keyring(kind.keyring_account());
        let _ = self.remove_file(kind.file_name());
        Ok(())
    }

    fn keyring_entry(&self, account: &str) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(&self.keyring_service, account)
            .map_err(|err| CredentialError::Message(err.to_string()))
    }

    fn get_from_keyring(&self, account: &str) -> Result<Option<String>, CredentialError> {
        let entry = self.keyring_entry(account)?;
        match entry.get_password() {
            Ok(value) => {
                let value = value.trim().to_string();
                if value.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(value))
                }
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(CredentialError::Message(err.to_string())),
        }
    }

    fn set_in_keyring(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let entry = self.keyring_entry(account)?;
        entry
            .set_password(secret)
            .map_err(|err| CredentialError::Message(err.to_string()))
    }

    fn delete_from_keyring(&self, account: &str) -> Result<(), CredentialError> {
        let entry = self.keyring_entry(account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(CredentialError::Message(err.to_string())),
        }
    }

    fn file_path(&self, file_name: &str) -> PathBuf {
        self.credentials_dir.join(file_name)
    }

    fn get_from_file(&self, file_name: &str) -> Result<String, CredentialError> {
        let path = self.file_path(file_name);
        let bytes = fs::read(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                CredentialError::NotFound
            } else {
                CredentialError::Message(format!("read {}: {err}", path.display()))
            }
        })?;
        let plain = decrypt_blob(&bytes).map_err(CredentialError::Message)?;
        let value = String::from_utf8(plain)
            .map_err(|err| CredentialError::Message(format!("invalid UTF-8 credential: {err}")))?;
        let value = value.trim().to_string();
        if value.is_empty() {
            Err(CredentialError::NotFound)
        } else {
            Ok(value)
        }
    }

    fn set_in_file(&self, file_name: &str, secret: &str) -> Result<(), CredentialError> {
        fs::create_dir_all(&self.credentials_dir).map_err(|err| {
            CredentialError::Message(format!(
                "create credentials dir {}: {err}",
                self.credentials_dir.display()
            ))
        })?;
        let path = self.file_path(file_name);
        let blob = encrypt_blob(secret.as_bytes()).map_err(|err| CredentialError::Message(err))?;
        write_secret_file(&path, &blob)?;
        Ok(())
    }

    fn remove_file(&self, file_name: &str) -> Result<(), CredentialError> {
        let path = self.file_path(file_name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CredentialError::Message(format!(
                "remove {}: {err}",
                path.display()
            ))),
        }
    }

    /// A secret the user defined, found by `account` (an opaque name, e.g.
    /// `env/<node>/<name>`): the keyring first, else the encrypted file.
    /// Unlike a [`CredentialKind`] there is no environment fallback.
    pub fn get_named(&self, account: &str) -> Option<String> {
        if let Ok(Some(value)) = self.get_from_keyring(account) {
            return Some(value);
        }
        self.get_from_file(&named_file_name(account)).ok()
    }

    pub fn has_named(&self, account: &str) -> bool {
        self.get_named(account).is_some()
    }

    pub fn set_named(&self, account: &str, secret: &str) -> Result<CredentialBackend, CredentialError> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(CredentialError::Message("credential cannot be empty".into()));
        }
        let file = named_file_name(account);
        if self.set_in_keyring(account, secret).is_ok()
            && self
                .get_from_keyring(account)
                .ok()
                .flatten()
                .is_some_and(|stored| stored == secret)
        {
            let _ = self.remove_file(&file);
            return Ok(CredentialBackend::Keyring);
        }
        self.set_in_file(&file, secret)?;
        Ok(CredentialBackend::EncryptedFile)
    }

    pub fn delete_named(&self, account: &str) {
        let _ = self.delete_from_keyring(account);
        let _ = self.remove_file(&named_file_name(account));
    }
}

/// The encrypted file for a user-defined secret: a hash of its account, so
/// any name is a safe file name.
fn named_file_name(account: &str) -> String {
    let digest = Sha256::digest(account.as_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("named-{hex}.enc")
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), CredentialError> {
    fs::write(path, bytes)
        .map_err(|err| CredentialError::Message(format!("write {}: {err}", path.display())))?;
    restrict_file_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| CredentialError::Message(format!("chmod {}: {err}", path.display())))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<(), CredentialError> {
    Ok(())
}

fn encrypt_blob(plain: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        if let Ok(blob) = dpapi_protect(plain) {
            return Ok(blob);
        }
    }
    encrypt_with_machine_key(plain)
}

fn decrypt_blob(blob: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        if let Ok(plain) = dpapi_unprotect(blob) {
            return Ok(plain);
        }
    }
    if blob.starts_with(FILE_MAGIC) {
        return decrypt_with_machine_key(blob);
    }
    #[cfg(windows)]
    {
        return dpapi_unprotect(blob);
    }
    #[cfg(not(windows))]
    {
        Err("unrecognized credential blob".into())
    }
}

#[cfg(windows)]
fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    unsafe {
        CryptProtectData(
            &mut input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| format!("CryptProtectData failed: {err}"))?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let protected = slice.to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as _));
        Ok(protected)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    unsafe {
        CryptUnprotectData(
            &mut input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| format!("CryptUnprotectData failed: {err}"))?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let plain = slice.to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as _));
        Ok(plain)
    }
}

fn encrypt_with_machine_key(plain: &[u8]) -> Result<Vec<u8>, String> {
    let key = machine_derived_key()?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|err| format!("invalid cipher key: {err}"))?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plain)
        .map_err(|err| format!("encrypt credential: {err}"))?;
    let mut out = Vec::with_capacity(FILE_MAGIC.len() + nonce.len() + ciphertext.len());
    out.extend_from_slice(FILE_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt_with_machine_key(blob: &[u8]) -> Result<Vec<u8>, String> {
    let rest = blob
        .get(FILE_MAGIC.len()..)
        .ok_or_else(|| "credential blob too short".to_string())?;
    let (nonce, ciphertext) = rest
        .split_at_checked(12)
        .ok_or_else(|| "credential blob missing nonce".to_string())?;
    let key = machine_derived_key()?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|err| format!("invalid cipher key: {err}"))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|err| format!("decrypt credential: {err}"))
}

fn machine_derived_key() -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tod-credentials-v1\0");
    if let Ok(user) = std::env::var("USERNAME").or_else(|_| std::env::var("USER")) {
        hasher.update(user.as_bytes());
        hasher.update(b"\0");
    }
    if let Some(id) = machine_id() {
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
    }
    Ok(hasher.finalize().into())
}

fn machine_id() -> Option<String> {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").ok()
    }
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("IOPlatformUUID") {
                if let Some(uuid) = line.split('"').nth(3) {
                    return Some(uuid.to_string());
                }
            }
        }
        None
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

pub fn resolve_linear_api_key(store: &CredentialStore) -> Option<String> {
    store.get(CredentialKind::LinearApiKey)
}

/// How to authenticate to GitHub from here: through the sandbox's proxy
/// when it injects the token (whatever is stored), else the stored token.
/// `None` when neither.
pub fn resolve_github_auth(store: &CredentialStore) -> Option<crate::github::GithubAuth> {
    if store.github_via_proxy {
        return Some(crate::github::GithubAuth::Proxy);
    }
    store.get(CredentialKind::GithubToken).map(crate::github::GithubAuth::Token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CredentialStore` isolated from the user's real credentials: a unique temp
    /// credentials dir and a unique OS-keyring service name (never `KEYRING_SERVICE`).
    /// Keyring entries and the dir are removed on drop, including when a test panics.
    struct IsolatedStore {
        store: CredentialStore,
    }

    impl IsolatedStore {
        fn new() -> Self {
            let id = uuid::Uuid::new_v4();
            let dir = std::env::temp_dir().join(format!("tod-cred-{id}"));
            fs::create_dir_all(&dir).unwrap();
            Self {
                store: CredentialStore {
                    credentials_dir: dir,
                    keyring_service: format!("tod-test-{id}"),
                    github_via_proxy: false,
                },
            }
        }
    }

    impl Drop for IsolatedStore {
        fn drop(&mut self) {
            let _ = self.store.delete(CredentialKind::LinearApiKey);
            let _ = self.store.delete(CredentialKind::GithubToken);
            let _ = fs::remove_dir_all(&self.store.credentials_dir);
        }
    }

    #[test]
    fn encrypted_file_roundtrip() {
        let isolated = IsolatedStore::new();
        let store = &isolated.store;
        store
            .set_in_file(CredentialKind::LinearApiKey.file_name(), "lin_api_test")
            .unwrap();
        assert_eq!(
            store.get(CredentialKind::LinearApiKey).as_deref(),
            Some("lin_api_test")
        );
        store.delete(CredentialKind::LinearApiKey).unwrap();
        // Check the backends directly: `get` would also consult LINEAR_API_KEY.
        assert!(matches!(
            store.get_from_file(CredentialKind::LinearApiKey.file_name()),
            Err(CredentialError::NotFound)
        ));
        assert!(
            store
                .get_from_keyring(CredentialKind::LinearApiKey.keyring_account())
                .ok()
                .flatten()
                .is_none()
        );
    }

    #[test]
    fn machine_key_blob_roundtrip() {
        let plain = b"secret-value";
        let blob = encrypt_with_machine_key(plain).unwrap();
        let decoded = decrypt_with_machine_key(&blob).unwrap();
        assert_eq!(decoded, plain);
    }

    #[test]
    fn set_and_get_use_separate_keyring_entries() {
        // CredentialStore creates a fresh keyring::Entry on each call; the backend must
        // persist by service/user identity, not in-memory on the Entry handle.
        let isolated = IsolatedStore::new();
        let kind = CredentialKind::LinearApiKey;
        let set_entry = isolated.store.keyring_entry(kind.keyring_account()).unwrap();
        let get_entry = isolated.store.keyring_entry(kind.keyring_account()).unwrap();
        set_entry.set_password("separate-entry-roundtrip").unwrap();
        assert_eq!(
            get_entry.get_password().unwrap(),
            "separate-entry-roundtrip"
        );
    }

    #[test]
    fn set_get_roundtrip_uses_readable_backend() {
        // Windows Credential Manager can accept writes that are not readable back
        // via the keyring crate; set() must fall back to the encrypted file in that case.
        let isolated = IsolatedStore::new();
        let store = &isolated.store;
        let secret = format!("lin_test_{}", uuid::Uuid::new_v4());
        let backend = store.set(CredentialKind::LinearApiKey, &secret).unwrap();
        assert_eq!(
            store.get(CredentialKind::LinearApiKey).as_deref(),
            Some(secret.as_str()),
            "credential not readable after set (backend: {backend:?})"
        );
    }

    #[test]
    fn github_via_the_proxy_is_available_without_a_token() {
        let isolated = IsolatedStore::new();
        let store = isolated.store.clone().with_github_via_proxy(true);
        assert_eq!(resolve_github_auth(&store), Some(crate::github::GithubAuth::Proxy));
        assert_eq!(store.backend(CredentialKind::GithubToken), Some(CredentialBackend::Proxy));
        assert_eq!(store.get(CredentialKind::GithubToken).as_deref(), Some(GITHUB_PROXY_PLACEHOLDER));
        // Only GitHub goes through it.
        assert_ne!(store.backend(CredentialKind::LinearApiKey), Some(CredentialBackend::Proxy));
    }

    #[test]
    fn the_proxy_wins_over_a_stored_token() {
        let isolated = IsolatedStore::new();
        isolated.store.set_in_file(CredentialKind::GithubToken.file_name(), "ghp_stored").unwrap();
        let proxied = isolated.store.clone().with_github_via_proxy(true);
        assert_eq!(resolve_github_auth(&proxied), Some(crate::github::GithubAuth::Proxy));
        let direct = isolated.store.clone().with_github_via_proxy(false);
        assert_eq!(
            resolve_github_auth(&direct),
            Some(crate::github::GithubAuth::Token("ghp_stored".into()))
        );
    }
}
