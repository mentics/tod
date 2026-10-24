//! Databases a sandbox may log in to through this machine
//! (`doc/cloud-sandboxes/database-access.md`).
//!
//! A database is declared once, on this machine (`[[database]]` in
//! `sandboxes.toml`): where it is, who to log in as, and which AWS profile
//! signs the login token. A sandbox never holds that profile. It asks for a
//! login (`tod-cli db run`); this machine mints a short-lived RDS IAM token
//! and carries the connection to the database, which is where the database's
//! IP allowlist and VPN let it in.

use crate::config::Forward;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The loopback port in the sandbox a database is reached on, unless set.
/// Not 5432: a sandbox may run a Postgres of its own there.
pub const DEFAULT_SANDBOX_PORT: u16 = 15432;

/// One `[[database]]` entry of `sandboxes.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Database {
    /// What the agent asks for (`tod-cli db run prod -- psql`).
    pub name: String,
    /// The database server's own name, the one its TLS certificate is for,
    /// and the one the token is signed for (for RDS, the instance endpoint,
    /// not a CNAME to it). Connections from the sandbox use this name too,
    /// mapped to the loopback address there.
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The database user the IAM token logs in as.
    pub user: String,
    /// The database to connect to.
    pub dbname: String,
    /// The AWS region of the RDS instance, which signs the token.
    pub region: String,
    /// The AWS CLI profile (`aws sso login --profile …`) that signs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The loopback port in the sandbox. See [`DEFAULT_SANDBOX_PORT`].
    #[serde(default = "default_sandbox_port")]
    pub sandbox_port: u16,
}

fn default_port() -> u16 {
    5432
}

fn default_sandbox_port() -> u16 {
    DEFAULT_SANDBOX_PORT
}

impl Database {
    /// Refuses a declaration that could not work, or that would put something
    /// odd on a command line or in a sandbox's `/etc/hosts`.
    pub fn validate(&self) -> Result<()> {
        let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        ensure!(word(&self.name), "database name {:?}: letters, digits, '-', '_' and '.' only", self.name);
        ensure!(word(&self.host), "database {}: host {:?} is not a plain host name", self.name, self.host);
        ensure!(word(&self.region), "database {}: region {:?}", self.name, self.region);
        ensure!(!self.user.is_empty() && !self.user.contains(char::is_whitespace), "database {}: user {:?}", self.name, self.user);
        ensure!(!self.dbname.is_empty(), "database {}: dbname is empty", self.name);
        ensure!(self.sandbox_port >= 1024, "database {}: sandbox_port {} is below 1024", self.name, self.sandbox_port);
        Ok(())
    }

    /// The port forward that carries the sandbox's connections here.
    pub fn forward(&self) -> Forward {
        Forward { port: self.sandbox_port, target: format!("{}:{}", self.host, self.port) }
    }

    /// Where the RDS certificate authority bundle for [`Self::region`] is.
    pub fn ca_url(&self) -> String {
        let region = &self.region;
        if region.starts_with("us-gov-") {
            format!("https://truststore.pki.us-gov-west-1.rds.amazonaws.com/{region}/{region}-bundle.pem")
        } else {
            format!("https://truststore.pki.rds.amazonaws.com/{region}/{region}-bundle.pem")
        }
    }

    /// A login token for the database: 15 minutes, signed here with the
    /// AWS profile. Signing contacts nothing, but fails when the profile's
    /// session has expired, and says to sign in again.
    pub fn mint_token(&self) -> Result<String> {
        let aws = aws_bin()?;
        let mut command = Command::new(&aws);
        command.args(["rds", "generate-db-auth-token", "--hostname", &self.host, "--port"]);
        command.arg(self.port.to_string());
        command.args(["--username", &self.user, "--region", &self.region]);
        if let Some(profile) = &self.profile {
            command.args(["--profile", profile]);
        }
        let out = command
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run {}", aws.display()))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let login = match &self.profile {
                Some(p) => format!(" (try `aws sso login --profile {p}`)"),
                None => String::new(),
            };
            bail!("could not sign a login for database {}{login}: {why}", self.name);
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        ensure!(!token.is_empty(), "the AWS CLI printed no token for database {}", self.name);
        Ok(token)
    }

    /// Whether this declaration works from this machine: the AWS profile can
    /// sign a login now, and the database accepts a TCP connection from here.
    /// It logs in to nothing and sends no token. Says what it found.
    pub fn check(&self) -> Result<String> {
        use std::net::{TcpStream, ToSocketAddrs};
        self.validate()?;
        self.mint_token()?;
        let target = format!("{}:{}", self.host, self.port);
        let addr = target
            .to_socket_addrs()
            .with_context(|| format!("look up {target}"))?
            .next()
            .with_context(|| format!("{target} has no address"))?;
        TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(8))
            .with_context(|| format!("connect to {target} ({addr}) from this machine: is the VPN up, and is this machine allowlisted?"))?;
        Ok(format!(
            "ok: {} can sign a login as {}, and {target} accepts connections from this machine",
            self.profile.as_deref().map_or("the default AWS profile".to_string(), |p| format!("profile {p}")),
            self.user
        ))
    }

    /// The reply a sandbox gets to a login request.
    pub fn login(&self) -> Result<Login> {
        Ok(Login {
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.sandbox_port,
            user: self.user.clone(),
            dbname: self.dbname.clone(),
            ca_url: self.ca_url(),
            token: self.mint_token()?,
        })
    }
}

/// What `tod-cli db run` needs to connect: the name and loopback port to
/// connect to, who and where, the CA bundle to verify the server with, and
/// the token to log in with. Sent from this machine to the sandbox; the
/// token is good for 15 minutes and for this one user on this one database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub name: String,
    /// The server's real name, which the sandbox maps to loopback.
    pub host: String,
    /// The loopback port in the sandbox.
    pub port: u16,
    pub user: String,
    pub dbname: String,
    pub ca_url: String,
    pub token: String,
}

impl Login {
    /// Refuses a reply that would put something odd in `/etc/hosts`, an
    /// environment variable or a download: it comes over the network.
    pub fn validate(&self) -> Result<()> {
        let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        ensure!(word(&self.name) && word(&self.host), "the login reply names an odd database or host");
        ensure!(self.port >= 1024, "the login reply's port {} is below 1024", self.port);
        ensure!(
            self.ca_url.starts_with("https://truststore.pki.") && self.ca_url.ends_with("-bundle.pem") && !self.ca_url.contains(char::is_whitespace),
            "the login reply's CA bundle URL {:?} is not an RDS trust store",
            self.ca_url
        );
        ensure!(!self.token.is_empty() && !self.token.contains(['\n', '\0']), "the login reply holds no usable token");
        Ok(())
    }
}

/// The AWS CLI: on `PATH`, else where Homebrew and the installer put it (an
/// app started from the Dock has a short `PATH`).
fn aws_bin() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(if cfg!(windows) { "aws.exe" } else { "aws" });
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    for fixed in ["/opt/homebrew/bin/aws", "/usr/local/bin/aws", "/usr/bin/aws"] {
        let p = PathBuf::from(fixed);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("the AWS CLI (`aws`) is not installed on this machine, and database logins are signed with it")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database {
            name: "prod".into(),
            host: "x.cv9iw2uvo9td.us-gov-east-1.rds.amazonaws.com".into(),
            port: 5432,
            user: "joel@cofactr.com".into(),
            dbname: "monolith".into(),
            region: "us-gov-east-1".into(),
            profile: Some("p".into()),
            sandbox_port: DEFAULT_SANDBOX_PORT,
        }
    }

    #[test]
    fn a_database_is_forwarded_and_validated() {
        let d = db();
        d.validate().unwrap();
        assert_eq!(d.forward(), Forward { port: 15432, target: "x.cv9iw2uvo9td.us-gov-east-1.rds.amazonaws.com:5432".into() });
        assert!(d.ca_url().starts_with("https://truststore.pki.us-gov-west-1.rds.amazonaws.com/us-gov-east-1/"));
        let commercial = Database { region: "us-east-1".into(), ..db() };
        assert_eq!(commercial.ca_url(), "https://truststore.pki.rds.amazonaws.com/us-east-1/us-east-1-bundle.pem");
        for bad in [
            Database { name: "a b".into(), ..db() },
            Database { host: "h\n127.0.0.1 evil".into(), ..db() },
            Database { host: "a b".into(), ..db() },
            Database { sandbox_port: 80, ..db() },
            Database { dbname: String::new(), ..db() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
