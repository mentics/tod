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
}

impl CredentialKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::LinearApiKey => "Linear API key",
        }
    }

    fn keyring_account(self) -> &'static str {
        match self {
            Self::LinearApiKey => "linear_api_key",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::LinearApiKey => "linear_api_key.enc",
        }
    }

    fn env_var(self) -> Option<&'static str> {
        match self {
            Self::LinearApiKey => Some("LINEAR_API_KEY"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialBackend {
    Keyring,
    EncryptedFile,
    Environment,
}

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
}

impl CredentialStore {
    pub fn new(paths: &TodPaths) -> Self {
        Self {
            credentials_dir: paths.credentials_dir(),
        }
    }

    pub fn from_data_root(data_root: &Path) -> Self {
        Self {
            credentials_dir: data_root.join("credentials"),
        }
    }

    /// Read a credential using the most secure available source.
    pub fn get(&self, kind: CredentialKind) -> Option<String> {
        match self.get_from_keyring(kind) {
            Ok(Some(value)) => return Some(value),
            Ok(None) | Err(_) => {}
        }
        if let Ok(value) = self.get_from_file(kind) {
            return Some(value);
        }
        kind.env_var()
            .and_then(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub fn backend(&self, kind: CredentialKind) -> Option<CredentialBackend> {
        if self.get_from_keyring(kind).ok().flatten().is_some() {
            return Some(CredentialBackend::Keyring);
        }
        if self.file_path(kind).is_file() {
            return Some(CredentialBackend::EncryptedFile);
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

        if self.set_in_keyring(kind, secret).is_ok()
            && self
                .get_from_keyring(kind)
                .ok()
                .flatten()
                .is_some_and(|stored| stored == secret)
        {
            let _ = self.remove_file(kind);
            return Ok(CredentialBackend::Keyring);
        }

        self.set_in_file(kind, secret)?;
        Ok(CredentialBackend::EncryptedFile)
    }

    pub fn delete(&self, kind: CredentialKind) -> Result<(), CredentialError> {
        let _ = self.delete_from_keyring(kind);
        let _ = self.remove_file(kind);
        Ok(())
    }

    fn get_from_keyring(&self, kind: CredentialKind) -> Result<Option<String>, CredentialError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, kind.keyring_account())
            .map_err(|err| CredentialError::Message(err.to_string()))?;
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

    fn set_in_keyring(&self, kind: CredentialKind, secret: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, kind.keyring_account())
            .map_err(|err| CredentialError::Message(err.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|err| CredentialError::Message(err.to_string()))
    }

    fn delete_from_keyring(&self, kind: CredentialKind) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, kind.keyring_account())
            .map_err(|err| CredentialError::Message(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(CredentialError::Message(err.to_string())),
        }
    }

    fn file_path(&self, kind: CredentialKind) -> PathBuf {
        self.credentials_dir.join(kind.file_name())
    }

    fn get_from_file(&self, kind: CredentialKind) -> Result<String, CredentialError> {
        let path = self.file_path(kind);
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

    fn set_in_file(&self, kind: CredentialKind, secret: &str) -> Result<(), CredentialError> {
        fs::create_dir_all(&self.credentials_dir).map_err(|err| {
            CredentialError::Message(format!(
                "create credentials dir {}: {err}",
                self.credentials_dir.display()
            ))
        })?;
        let path = self.file_path(kind);
        let blob = encrypt_blob(secret.as_bytes()).map_err(|err| CredentialError::Message(err))?;
        write_secret_file(&path, &blob)?;
        Ok(())
    }

    fn remove_file(&self, kind: CredentialKind) -> Result<(), CredentialError> {
        let path = self.file_path(kind);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CredentialError::Message(format!(
                "remove {}: {err}",
                path.display()
            ))),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tod-cred-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let store = CredentialStore {
            credentials_dir: dir.clone(),
        };
        store
            .set_in_file(CredentialKind::LinearApiKey, "lin_api_test")
            .unwrap();
        assert_eq!(
            store.get(CredentialKind::LinearApiKey).as_deref(),
            Some("lin_api_test")
        );
        store.delete(CredentialKind::LinearApiKey).unwrap();
        assert!(store.get(CredentialKind::LinearApiKey).is_none());
        let _ = fs::remove_dir_all(dir);
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
        let set_entry = keyring::Entry::new(KEYRING_SERVICE, "linear_api_key").unwrap();
        let get_entry = keyring::Entry::new(KEYRING_SERVICE, "linear_api_key").unwrap();
        let _ = set_entry.delete_credential();
        set_entry.set_password("separate-entry-roundtrip").unwrap();
        assert_eq!(
            get_entry.get_password().unwrap(),
            "separate-entry-roundtrip"
        );
        let _ = set_entry.delete_credential();
    }

    #[test]
    fn set_get_roundtrip_uses_readable_backend() {
        // Windows Credential Manager can accept writes that are not readable back
        // via the keyring crate; set() must fall back to the encrypted file in that case.
        let dir = std::env::temp_dir().join(format!("tod-cred-kr-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let store = CredentialStore {
            credentials_dir: dir.clone(),
        };
        let _ = store.delete(CredentialKind::LinearApiKey);
        let secret = format!("lin_test_{}", uuid::Uuid::new_v4());
        let backend = store.set(CredentialKind::LinearApiKey, &secret).unwrap();
        assert_eq!(
            store.get(CredentialKind::LinearApiKey).as_deref(),
            Some(secret.as_str()),
            "credential not readable after set (backend: {backend:?})"
        );
        store.delete(CredentialKind::LinearApiKey).unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
