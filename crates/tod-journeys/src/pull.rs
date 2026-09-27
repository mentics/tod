//! `tod-journeys pull` (spec §9.5, §9.3): poll the inbox topic, buffer split
//! parts, join and decrypt complete bundles, file them, and acknowledge.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tod_integration::ntfy::{self, Message};

use crate::config::{self, Config};

/// Parses `<bundle-id>.journey.age` or `<bundle-id>.<n>-of-<m>.journey.age`
/// (spec §9.3) into the bundle id and, for a split part, its 1-based part
/// number and the total part count.
pub fn parse_attachment_name(name: &str) -> Option<(String, Option<(u32, u32)>)> {
    let stem = name.strip_suffix(".journey.age")?;
    if let Some((bundle_id, part)) = stem.rsplit_once('.') {
        if let Some((n, m)) = part.split_once("-of-") {
            if let (Ok(n), Ok(m)) = (n.parse::<u32>(), m.parse::<u32>()) {
                if n >= 1 && m >= 1 && n <= m {
                    return Some((bundle_id.to_string(), Some((n, m))));
                }
            }
        }
    }
    Some((stem.to_string(), None))
}

fn received_path(home: &Path, bundle_id: &str) -> PathBuf {
    config::received_dir(home).join(format!("{bundle_id}.journey"))
}

/// What happened to one polled message.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No attachment, or a name that didn't match the convention.
    Ignored,
    /// A part was buffered but the bundle isn't complete yet.
    PartBuffered,
    /// The bundle was already in `received/`; acknowledged again without
    /// downloading.
    AlreadyReceived { bundle_id: String },
    /// All parts arrived, joined, decrypted, and filed.
    Completed { bundle_id: String },
}

/// Handles one polled message: downloads its attachment (if any and not
/// already received), buffers split parts on disk under `<home>/parts/`,
/// and once a bundle is complete, joins, opens, decompresses, files it under
/// `<home>/received/`, and acknowledges it. Takes `download`/`ack` as
/// closures so tests can feed synthetic data without a network call.
pub fn process_message(
    home: &Path,
    identity: &str,
    msg: &Message,
    download: impl Fn(&str) -> Result<Vec<u8>>,
    ack: impl Fn(&str) -> Result<()>,
) -> Result<Outcome> {
    let Some(attachment) = &msg.attachment else {
        return Ok(Outcome::Ignored);
    };
    let Some((bundle_id, part)) = parse_attachment_name(&attachment.name) else {
        return Ok(Outcome::Ignored);
    };

    let dest = received_path(home, &bundle_id);
    if dest.exists() {
        ack(&format!("got {bundle_id}"))?;
        return Ok(Outcome::AlreadyReceived { bundle_id });
    }

    let bytes = download(&attachment.url)?;

    let (n, total) = part.unwrap_or((1, 1));
    let parts_dir = config::parts_dir(home).join(&bundle_id);
    std::fs::create_dir_all(&parts_dir)?;
    std::fs::write(parts_dir.join(n.to_string()), &bytes)?;

    let have = (1..=total)
        .filter(|i| parts_dir.join(i.to_string()).exists())
        .count() as u32;
    if have != total {
        return Ok(Outcome::PartBuffered);
    }

    let mut ordered = Vec::with_capacity(total as usize);
    for i in 1..=total {
        ordered.push(std::fs::read(parts_dir.join(i.to_string()))?);
    }
    let sealed = tod_journey::seal::join(ordered);
    let opened = tod_journey::seal::open(identity, &sealed)
        .and_then(|compressed| Ok(tod_journey::bundle::decompress(&compressed)?));
    let plain = match opened {
        Ok(plain) => plain,
        Err(e) => {
            // A bundle that cannot be opened will never become openable, and
            // `last_seen` moves past it, so drop its buffered parts.
            let _ = std::fs::remove_dir_all(&parts_dir);
            return Err(e);
        }
    };

    std::fs::create_dir_all(config::received_dir(home))?;
    std::fs::write(&dest, &plain)?;
    let _ = std::fs::remove_dir_all(&parts_dir);

    ack(&format!("got {bundle_id}"))?;
    Ok(Outcome::Completed { bundle_id })
}

/// Polls the inbox once, processing every message, advancing and saving
/// `last_seen` after each. With `once`, returns after this pass; otherwise
/// loops every 60 seconds (spec §9.5).
pub fn run_pull(cfg: &mut Config, identity: &str, home: &Path, once: bool) -> Result<()> {
    loop {
        pull_once(cfg, identity, home)?;
        if once {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn pull_once(cfg: &mut Config, identity: &str, home: &Path) -> Result<()> {
    let since = cfg.last_seen.clone().unwrap_or_else(|| "all".to_string());
    let messages = ntfy::poll(&cfg.server, &cfg.inbox, &since)?;
    for msg in &messages {
        let server = cfg.server.clone();
        let ack_topic = cfg.ack.clone();
        match process_message(
            home,
            identity,
            msg,
            |url| ntfy::download(url),
            |text| ntfy::publish_text(&server, &ack_topic, text),
        ) {
            Ok(_) => {}
            Err(e) => eprintln!("tod-journeys: pull: {e:#}"),
        }
        cfg.last_seen = Some(msg.id.clone());
        cfg.save(home)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tod_integration::ntfy::Attachment;
    use tod_journey::{bundle::BundleWriter, seal, Actor, Event, JourneyReader};

    fn make_bundle_bytes() -> Vec<u8> {
        let mut w = BundleWriter::new().unwrap();
        w.append(Actor::User, Event::Milestone { state: "sent".into() })
            .unwrap();
        w.finish().unwrap()
    }

    fn msg(id: &str, name: &str, url: &str) -> Message {
        Message {
            id: id.to_string(),
            time: 0,
            message: String::new(),
            attachment: Some(Attachment {
                name: name.to_string(),
                url: url.to_string(),
                size: 0,
                expires: 0,
            }),
        }
    }

    #[test]
    fn parses_single_and_split_names() {
        assert_eq!(
            parse_attachment_name("abc-123.journey.age"),
            Some(("abc-123".to_string(), None))
        );
        assert_eq!(
            parse_attachment_name("abc-123.2-of-3.journey.age"),
            Some(("abc-123".to_string(), Some((2, 3))))
        );
        assert_eq!(parse_attachment_name("not-a-journey.txt"), None);
    }

    #[test]
    fn assembles_a_split_bundle_and_acknowledges_once_complete() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let (identity, recipient) = tod_journey::generate_identity();
        let bundle_id = "bundle-xyz";
        let bundle_bytes = make_bundle_bytes();
        let sealed = seal::seal(&recipient, &bundle_bytes).unwrap();
        // Force at least two parts.
        let parts = seal::split(&sealed, (sealed.len() / 2).max(1));
        assert!(parts.len() >= 2);
        let total = parts.len() as u32;

        let downloads: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let acks: RefCell<Vec<String>> = RefCell::new(Vec::new());

        let download = |url: &str| {
            downloads.borrow_mut().push(url.to_string());
            let n: usize = url.strip_prefix("part://").unwrap().parse().unwrap();
            Ok(parts[n - 1].clone())
        };
        let ack = |text: &str| {
            acks.borrow_mut().push(text.to_string());
            Ok(())
        };

        let mut last_outcome = Outcome::Ignored;
        for n in 1..=total {
            let name = format!("{bundle_id}.{n}-of-{total}.journey.age");
            let url = format!("part://{n}");
            let m = msg(&format!("id-{n}"), &name, &url);
            last_outcome = process_message(&home, identity.trim(), &m, &download, &ack).unwrap();
            if n < total {
                assert_eq!(last_outcome, Outcome::PartBuffered);
                assert!(acks.borrow().is_empty());
            }
        }
        assert_eq!(
            last_outcome,
            Outcome::Completed { bundle_id: bundle_id.to_string() }
        );
        assert_eq!(acks.borrow().as_slice(), [format!("got {bundle_id}")]);
        assert_eq!(downloads.borrow().len() as u32, total);

        let received = config::received_dir(&home).join(format!("{bundle_id}.journey"));
        assert!(received.exists());
        let plain = std::fs::read(&received).unwrap();
        let records: Vec<_> = JourneyReader::from_reader(plain.as_slice()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event, Event::Milestone { state: "sent".into() });

        // The parts buffer is cleaned up once the bundle is complete.
        assert!(!config::parts_dir(&home).join(bundle_id).exists());
    }

    #[test]
    fn a_resend_of_an_already_received_bundle_is_acknowledged_without_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let received_dir = config::received_dir(&home);
        std::fs::create_dir_all(&received_dir).unwrap();
        std::fs::write(received_dir.join("dup-1.journey"), b"already here").unwrap();

        let downloads: RefCell<u32> = RefCell::new(0);
        let acks: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let download = |_: &str| {
            *downloads.borrow_mut() += 1;
            Ok(Vec::new())
        };
        let ack = |text: &str| {
            acks.borrow_mut().push(text.to_string());
            Ok(())
        };

        let m = msg("id-1", "dup-1.journey.age", "part://1");
        let outcome = process_message(&home, "unused-identity", &m, download, ack).unwrap();

        assert_eq!(outcome, Outcome::AlreadyReceived { bundle_id: "dup-1".to_string() });
        assert_eq!(*downloads.borrow(), 0);
        assert_eq!(acks.borrow().as_slice(), ["got dup-1".to_string()]);
    }

    #[test]
    fn a_message_without_an_attachment_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let m = Message { id: "id-1".to_string(), time: 0, message: "got 42".to_string(), attachment: None };
        let outcome = process_message(&home, "unused", &m, |_| Ok(Vec::new()), |_| Ok(())).unwrap();
        assert_eq!(outcome, Outcome::Ignored);
    }

    #[test]
    fn an_unopenable_bundle_leaves_no_buffered_parts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let (identity, _recipient) = tod_journey::seal::generate_test_identity();
        let m = msg("id-1", "junk.journey.age", "part://1");
        let err = process_message(&home, &identity, &m, |_| Ok(b"not sealed".to_vec()), |_| Ok(()));
        assert!(err.is_err());
        assert!(!config::parts_dir(&home).join("junk").exists());
    }
}
