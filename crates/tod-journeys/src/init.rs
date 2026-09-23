//! `tod-journeys init` (spec §9.5): generate an age identity, two random
//! topic names, and print the relay code to paste into the app's settings.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tod_journey::relay_code::RelayCode;

use crate::config::{self, Config};

pub fn run(home: &Path, server: &str, force: bool) -> Result<()> {
    fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;

    let identity_path = config::identity_path(home);
    if identity_path.exists() && !force {
        bail!(
            "an identity already exists at {} — pass --force to overwrite (this abandons any journeys sealed to the old key)",
            identity_path.display()
        );
    }

    let (identity, recipient) = tod_journey::generate_identity();
    fs::write(&identity_path, &identity)
        .with_context(|| format!("writing {}", identity_path.display()))?;

    let inbox = config::random_topic();
    let ack = config::random_topic();
    let cfg = Config {
        server: server.to_string(),
        inbox: inbox.clone(),
        ack: ack.clone(),
        last_seen: None,
    };
    cfg.save(home)?;

    let code = RelayCode {
        recipient,
        server: server.to_string(),
        inbox,
        ack,
    };
    println!("{}", code.format()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::seal;

    #[test]
    fn init_writes_identity_and_prints_a_valid_relay_code() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        run(&home, "https://ntfy.sh", false).unwrap();

        let identity = fs::read_to_string(config::identity_path(&home)).unwrap();
        assert!(identity.starts_with("AGE-SECRET-KEY-"));

        let cfg = Config::load(&home).unwrap();
        assert_eq!(cfg.server, "https://ntfy.sh");
        assert_eq!(cfg.inbox.len(), 32);
        assert_eq!(cfg.ack.len(), 32);
        assert_ne!(cfg.inbox, cfg.ack);
    }

    #[test]
    fn relay_code_recipient_round_trips_with_the_saved_identity() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");

        // Capture stdout is awkward in a unit test, so exercise the same
        // path init uses directly and check the round trip.
        fs::create_dir_all(&home).unwrap();
        let (identity, recipient) = tod_journey::generate_identity();
        fs::write(config::identity_path(&home), &identity).unwrap();
        let code = RelayCode {
            recipient: recipient.clone(),
            server: "https://ntfy.sh".to_string(),
            inbox: config::random_topic(),
            ack: config::random_topic(),
        };
        let formatted = code.format().unwrap();

        let parsed = RelayCode::parse(&formatted).unwrap();
        assert_eq!(parsed.recipient, recipient);

        let sealed = seal::seal(&parsed.recipient, b"hello receiver").unwrap();
        let opened = seal::open(identity.trim(), &sealed).unwrap();
        assert_eq!(opened, b"hello receiver");
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        run(&home, "https://ntfy.sh", false).unwrap();
        assert!(run(&home, "https://ntfy.sh", false).is_err());
        assert!(run(&home, "https://ntfy.sh", true).is_ok());
    }
}
