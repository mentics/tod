//! Whether the GitHub token tod holds works: present, and accepted by GitHub.
//!
//! Everything that talks to GitHub (`tod-cli pr`, the pull request views, the
//! gates, the PR babysitter) uses this one token. When it is missing or
//! rejected those all fail the same way, so the app checks it once, shows the
//! result where the user cannot miss it, and agents are told to hand the
//! problem to the user instead of working around it.

use crate::credentials::{CredentialBackend, CredentialKind, CredentialStore};
use crate::github::{Github, GithubAuth, GithubError};

/// Where the user creates a token.
pub const NEW_TOKEN_URL: &str = "https://github.com/settings/tokens/new?scopes=repo&description=tod";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStatus {
    /// An autonomous node's sandbox, whose proxy adds the token.
    Proxy,
    /// Nothing is stored and `GITHUB_TOKEN` is unset. `keyring_problem` is
    /// why the OS keyring could not be read, if that is why.
    Missing { keyring_problem: Option<String> },
    /// GitHub answered 401: the token is expired, revoked, or mistyped.
    Rejected { source: CredentialBackend },
    /// GitHub could not be asked (offline, rate limited, 5xx); the token is
    /// neither confirmed nor ruled out.
    Unverified { source: CredentialBackend, detail: String },
    Valid { login: String, source: CredentialBackend },
}

impl TokenStatus {
    /// The user must act: the token is missing or rejected.
    pub fn needs_fixing(&self) -> bool {
        matches!(self, Self::Missing { .. } | Self::Rejected { .. })
    }

    /// One line for a banner.
    pub fn headline(&self) -> String {
        match self {
            Self::Proxy => "GitHub is authenticated by the sandbox's proxy".into(),
            Self::Missing { .. } => {
                "No GitHub token: agents cannot read or open pull requests".into()
            }
            Self::Rejected { source } => format!(
                "GitHub rejected the stored token ({}): agents cannot read or open pull requests",
                source_label(*source)
            ),
            Self::Unverified { detail, .. } => format!("Could not check the GitHub token: {detail}"),
            Self::Valid { login, .. } => format!("GitHub token works (signed in as {login})"),
        }
    }

    /// What the user can do about it, or more about why.
    pub fn help(&self) -> String {
        match self {
            Self::Missing { keyring_problem: Some(problem) } => format!(
                "The OS keyring could not be read ({problem}), so a token stored there looks \
                 missing. Allow tod access to the keychain, or add the token again."
            ),
            Self::Missing { keyring_problem: None } => {
                "Add a token (classic with `repo` scope, or fine-grained with Pull requests and \
                 Contents read/write), or set GITHUB_TOKEN where tod is launched."
                    .into()
            }
            Self::Rejected { source: CredentialBackend::Environment } => {
                "The token comes from the GITHUB_TOKEN environment variable. Replace it there, \
                 or add a token in tod, which takes precedence."
                    .into()
            }
            Self::Rejected { .. } => {
                "It was revoked, expired, or lost its access. Create a new token and replace it."
                    .into()
            }
            Self::Unverified { .. } | Self::Valid { .. } | Self::Proxy => String::new(),
        }
    }

    /// What an agent that hit this should say and do: it cannot fix it, and
    /// retrying or reaching for another credential will not help.
    pub fn for_agent(&self) -> String {
        format!(
            "{}. The user must fix this: tod shows a red banner with an \"Add token\" / \
             \"Replace token\" button. Stop the work that needs GitHub, tell the user it is \
             blocked on that, and do not retry, search for other tokens, or use `gh auth`.",
            self.headline()
        )
    }
}

fn source_label(source: CredentialBackend) -> &'static str {
    match source {
        CredentialBackend::Keyring => "keyring",
        CredentialBackend::EncryptedFile => "encrypted file",
        CredentialBackend::Environment => "GITHUB_TOKEN",
        CredentialBackend::Proxy => "sandbox proxy",
    }
}

/// The login `token` belongs to, if GitHub accepts it.
pub fn verify_token(token: &str) -> Result<String, GithubError> {
    Github::new(GithubAuth::Token(token.trim().to_string())).user_login()
}

/// Check the token `store` holds against GitHub. Makes a network call.
pub fn check(store: &CredentialStore) -> TokenStatus {
    let Some(source) = store.backend(CredentialKind::GithubToken) else {
        return TokenStatus::Missing { keyring_problem: store.keyring_problem(CredentialKind::GithubToken) };
    };
    if source == CredentialBackend::Proxy {
        return TokenStatus::Proxy;
    }
    let Some(token) = store.get(CredentialKind::GithubToken) else {
        return TokenStatus::Missing { keyring_problem: store.keyring_problem(CredentialKind::GithubToken) };
    };
    match verify_token(&token) {
        Ok(login) => TokenStatus::Valid { login, source },
        Err(GithubError::Unauthorized) => TokenStatus::Rejected { source },
        Err(other) => TokenStatus::Unverified { source, detail: other.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_missing_and_rejected_need_the_user() {
        let src = CredentialBackend::Keyring;
        assert!(TokenStatus::Missing { keyring_problem: None }.needs_fixing());
        assert!(TokenStatus::Rejected { source: src }.needs_fixing());
        assert!(!TokenStatus::Unverified { source: src, detail: "offline".into() }.needs_fixing());
        assert!(!TokenStatus::Valid { login: "a".into(), source: src }.needs_fixing());
        assert!(!TokenStatus::Proxy.needs_fixing());
    }

    #[test]
    fn agent_message_hands_the_problem_to_the_user() {
        let msg = TokenStatus::Rejected { source: CredentialBackend::EncryptedFile }.for_agent();
        assert!(msg.contains("rejected") && msg.contains("banner") && msg.contains("do not retry"));
    }

    #[test]
    fn a_stored_token_missing_from_every_source_is_missing() {
        // An isolated environment: no GITHUB_TOKEN, an empty credentials dir.
        let dir = std::env::temp_dir().join(format!("tod-ghtok-{}", uuid::Uuid::new_v4()));
        let store = CredentialStore::from_data_root(&dir).with_github_via_proxy(false);
        if std::env::var("GITHUB_TOKEN").is_ok_and(|v| !v.trim().is_empty()) {
            return; // the developer's own environment would answer
        }
        assert!(matches!(check(&store), TokenStatus::Missing { .. }));
    }
}
