//! Signing in to apps behind Cloudflare Access (Dagster, the admin console)
//! so cloud nodes can reach them as the user.
//!
//! `cloudflared access login <app>` opens the user's browser (Google, or
//! silently under WARP) and `cloudflared access token -app=<app>` prints the
//! signed token Access issued. It is kept like any other credential
//! ([`CredentialStore`]: keyring, else the encrypted file) and a node's
//! sandbox proxy sends it as `cf-access-token` to that host, so no agent ever
//! holds it. It lasts as long as the Access application's session duration
//! (the token's own `exp`), and is renewed by signing in again; there is no
//! headless refresh, so an expired token is shown as expired, never dropped.

use crate::credentials::CredentialStore;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use std::process::{Command, Stdio};

/// The header Access reads the token from.
pub const HEADER: &str = "cf-access-token";
const INDEX: &str = "cloudflare_access/apps";

fn account(host: &str) -> String {
    format!("cloudflare_access/{host}")
}

/// One app the user has signed in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStatus {
    pub host: String,
    /// Unix seconds the token stops working, from its own claims.
    pub expires_at: Option<i64>,
}

impl AppStatus {
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }
}

/// The host of an app URL or bare host (`https://dagster.cofactr.com/x` → `dagster.cofactr.com`).
pub fn app_host(input: &str) -> Result<String> {
    let host = crate::environment::host_of(input).ok_or_else(|| anyhow!("{input:?} is not an app URL"))?;
    if host.contains(['*', '@', ':']) || !host.contains('.') {
        bail!("{host:?} must be one app's exact host, e.g. dagster.cofactr.com");
    }
    Ok(host)
}

/// The `exp` claim of a JWT, unverified (Access verifies it; we only report it).
pub fn expiry(token: &str) -> Option<i64> {
    let payload = token.trim().split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get("exp")?.as_i64()
}

fn looks_like_token(token: &str) -> bool {
    let t = token.trim();
    t.split('.').count() == 3 && t.len() > 40 && !t.contains(char::is_whitespace)
}

fn hosts(store: &CredentialStore) -> Vec<String> {
    store.get_named(INDEX).map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default()
}

/// Store `token` for `host`, adding it to the list of apps. Touches the
/// keyring: never on the UI thread.
pub fn save(store: &CredentialStore, host: &str, token: &str) -> Result<AppStatus> {
    if !looks_like_token(token) {
        bail!("that is not a Cloudflare Access token (three dot-separated parts)");
    }
    store.set_named(&account(host), token).map_err(|e| anyhow!("could not store the token: {e}"))?;
    let mut all = hosts(store);
    if !all.iter().any(|h| h == host) {
        all.push(host.to_string());
        store.set_named(INDEX, &all.join(" ")).map_err(|e| anyhow!("could not store the list: {e}"))?;
    }
    Ok(AppStatus { host: host.to_string(), expires_at: expiry(token) })
}

/// Forget the sign-in for `host`. Returns whether there was one.
pub fn remove(store: &CredentialStore, host: &str) -> bool {
    let all = hosts(store);
    let had = all.iter().any(|h| h == host);
    store.delete_named(&account(host));
    let rest: Vec<String> = all.into_iter().filter(|h| h != host).collect();
    if rest.is_empty() {
        store.delete_named(INDEX);
    } else if let Err(e) = store.set_named(INDEX, &rest.join(" ")) {
        eprintln!("could not update the list of Cloudflare Access apps: {e}");
    }
    had
}

/// Every app signed in to, with when its token expires.
pub fn status(store: &CredentialStore) -> Vec<AppStatus> {
    hosts(store)
        .into_iter()
        .map(|host| {
            let expires_at = store.get_named(&account(&host)).as_deref().and_then(expiry);
            AppStatus { host, expires_at }
        })
        .collect()
}

/// The proxy credentials for a node's sandbox: each signed-in app's token on
/// its own host. An expired token is still sent (Access answers with its login
/// page); [`status`] is where it shows as expired. Reads the credential store:
/// never on the UI thread.
pub fn credentials(store: &CredentialStore) -> Vec<tod_sandbox::node::CustomCredential> {
    hosts(store)
        .into_iter()
        .filter_map(|host| {
            let token = store.get_named(&account(&host))?;
            Some(tod_sandbox::node::CustomCredential {
                name: format!("cloudflare_access_{}", host.replace(['.', '-'], "_")),
                hosts: vec![host],
                header: HEADER.to_string(),
                template: "{value}".to_string(),
                secret_value: token.trim().to_string(),
            })
        })
        .collect()
}

/// Sign in to `app` with `cloudflared` and store the token. Opens the user's
/// browser and waits for them: slow and interactive, never on the UI thread.
/// `cloudflared`'s own messages (the login URL) go to the terminal.
pub fn login(store: &CredentialStore, app: &str) -> Result<AppStatus> {
    let host = app_host(app)?;
    let url = format!("https://{host}");
    let missing = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!("`cloudflared` is not installed (macOS: `brew install cloudflared`)")
        } else {
            anyhow!(e).context("could not run cloudflared")
        }
    };
    let status = Command::new("cloudflared")
        .args(["access", "login", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map_err(missing)?;
    if !status.success() {
        bail!("`cloudflared access login {url}` failed ({status}); is {host} behind Cloudflare Access?");
    }
    let out = Command::new("cloudflared")
        .args(["access", "token", &format!("-app={url}")])
        .output()
        .map_err(missing)?;
    if !out.status.success() {
        bail!(
            "`cloudflared access token` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    save(store, &host, &token).with_context(|| format!("cloudflared printed something for {host} that is not a token"))
}

/// Sign in again to every app already signed in to (renewing before the
/// sessions lapse), one after the other. Each is one browser round trip, and
/// silent while the user's identity session lasts. Returns each app's outcome,
/// so one failure does not hide the others.
pub fn login_all(store: &CredentialStore) -> Vec<(String, Result<AppStatus>)> {
    hosts(store).into_iter().map(|h| (h.clone(), login(store, &h))).collect()
}

/// Whether `data_root`'s store holds a sign-in for `host` (for tests and checks).
pub fn has(store: &CredentialStore, host: &str) -> bool {
    store.has_named(&account(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(exp: i64) -> String {
        let b = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.{}", b(r#"{"alg":"RS256"}"#), b(&format!(r#"{{"exp":{exp},"aud":["x"]}}"#)), b("signature-bytes-here-long-enough"))
    }

    #[test]
    fn the_expiry_is_the_tokens_exp_claim() {
        assert_eq!(expiry(&jwt(1_800_000_000)), Some(1_800_000_000));
        assert_eq!(expiry("not a token"), None);
        assert!(looks_like_token(&jwt(1)));
        assert!(!looks_like_token("sk-ant-abc"));
    }

    #[test]
    fn an_app_is_one_exact_host() {
        assert_eq!(app_host("https://Dagster.cofactr.com/graphql").unwrap(), "dagster.cofactr.com");
        assert!(app_host("*.cofactr.com").is_err());
        assert!(app_host("").is_err());
    }

    #[test]
    fn expired_is_reported_not_hidden() {
        let s = AppStatus { host: "a.b.com".into(), expires_at: Some(100) };
        assert!(s.expired(101) && !s.expired(99));
        assert!(!AppStatus { host: "a.b.com".into(), expires_at: None }.expired(1));
    }
}
