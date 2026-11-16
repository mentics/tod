//! Copy or move a node's environment (variables and secrets, values
//! included) to another node, in bulk.
//!
//! Only the entries a node defines itself move: an inherited entry belongs to
//! the ancestor that defines it. A name the target already defines is a
//! *conflict*, settled for the whole transfer by [`OnConflict`]: skip it (the
//! target keeps its own) or overwrite it (the target gets the source's
//! definition and, for a secret, its stored value). Secret values go through
//! the credential store and are never shown. A move deletes from the source
//! only what actually reached the target.

use crate::credentials::CredentialStore;
use crate::environment::{self, Entry, EntryKind};
use crate::fleet::FleetStore;
use anyhow::{Result, bail};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

/// What to do with a name the target already defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    Skip,
    Overwrite,
}

/// One entry of the source and what a transfer would do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub secret: bool,
    /// The target defines this name itself.
    pub conflict: bool,
    /// A secret with a stored value on the source (always true for a variable).
    pub has_value: bool,
}

/// What a transfer did, by entry name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// New on the target.
    pub added: Vec<String>,
    /// Replaced the target's own definition.
    pub overwritten: Vec<String>,
    /// Left alone because the target defines it and the policy was skip.
    pub skipped: Vec<String>,
    /// Secrets whose entry moved but that had no stored value to move.
    pub without_value: Vec<String>,
    /// A move: the names removed from the source.
    pub removed_from_source: Vec<String>,
}

impl Outcome {
    pub fn transferred(&self) -> usize {
        self.added.len() + self.overwritten.len()
    }

    pub fn summary(&self, mode: Mode) -> String {
        let verb = if mode == Mode::Move { "moved" } else { "copied" };
        let mut s = format!("{} {verb}", self.transferred());
        if !self.overwritten.is_empty() {
            s.push_str(&format!(" ({} overwritten)", self.overwritten.len()));
        }
        if !self.skipped.is_empty() {
            s.push_str(&format!(", {} skipped: {}", self.skipped.len(), self.skipped.join(", ")));
        }
        if !self.without_value.is_empty() {
            s.push_str(&format!(
                "; no stored value for {} (set it on the target)",
                self.without_value.join(", ")
            ));
        }
        s
    }
}

/// The merged entry list for the target, and the outcome it implies. Pure.
/// `names` limits the transfer to those entries (case-insensitive); `None`
/// takes every one the source defines.
pub fn merge(
    source: &[Entry],
    target: &[Entry],
    names: Option<&[String]>,
    on_conflict: OnConflict,
) -> Result<(Vec<Entry>, Outcome, Vec<Entry>)> {
    if let Some(names) = names {
        for n in names {
            if !source.iter().any(|e| e.name.eq_ignore_ascii_case(n)) {
                bail!("`{n}` is not defined on the source node itself (an inherited entry stays where it is defined)");
            }
        }
    }
    let picked: Vec<&Entry> = source
        .iter()
        .filter(|e| names.is_none_or(|ns| ns.iter().any(|n| n.eq_ignore_ascii_case(&e.name))))
        .collect();
    let mut merged = target.to_vec();
    let mut out = Outcome::default();
    let mut taken = Vec::new();
    for entry in picked {
        match merged.iter().position(|t| t.name.eq_ignore_ascii_case(&entry.name)) {
            Some(i) if on_conflict == OnConflict::Skip => {
                let _ = i;
                out.skipped.push(entry.name.clone());
            }
            Some(i) => {
                merged[i] = entry.clone();
                out.overwritten.push(entry.name.clone());
                taken.push(entry.clone());
            }
            None => {
                merged.push(entry.clone());
                out.added.push(entry.name.clone());
                taken.push(entry.clone());
            }
        }
    }
    Ok((merged, out, taken))
}

/// What a transfer from `from` to `to` would touch, for the user to review.
/// Reads the credential store: never on the UI thread.
pub fn plan(fleet: &FleetStore, creds: &CredentialStore, from: Uuid, to: Uuid) -> Result<Vec<Item>> {
    if from == to {
        bail!("the source and the target are the same node");
    }
    let (source, target) = fleet.read(|conn| Ok((environment::entries(conn, from)?, environment::entries(conn, to)?)))?;
    Ok(source
        .iter()
        .map(|e| Item {
            name: e.name.clone(),
            secret: e.kind == EntryKind::Secret,
            conflict: target.iter().any(|t| t.name.eq_ignore_ascii_case(&e.name)),
            has_value: e.kind == EntryKind::Variable
                || creds.has_named(&environment::secret_account(from, &e.name)),
        })
        .collect())
}

/// Copy or move `names` (all, when `None`) from `from` to `to`. Enables the
/// Environment capability on the target if it lacks it. Waits for the writes,
/// so a caller reads the result straight away; reads and writes the credential
/// store: never on the UI thread. Nothing changes on the source until the
/// target holds everything.
pub fn transfer(
    fleet: &FleetStore,
    creds: &CredentialStore,
    from: Uuid,
    to: Uuid,
    names: Option<&[String]>,
    mode: Mode,
    on_conflict: OnConflict,
) -> Result<Outcome> {
    use crate::outline::OutlineMutation;
    use crate::outline::repos::node::NodeRepo;
    use crate::outline::types::Capability;

    transfer_with(
        creds,
        from,
        to,
        names,
        mode,
        on_conflict,
        || {
            fleet.read(|conn| {
                let enabled = NodeRepo::new(conn).list_capabilities(to)?.contains(&Capability::Environment);
                Ok((environment::entries(conn, from)?, environment::entries(conn, to)?, enabled))
            })
        },
        |mutation: OutlineMutation| {
            fleet.enqueue_outline(mutation)?;
            fleet.writer().flush()?;
            Ok(())
        },
    )
}

/// [`transfer`] over any way of reading the two nodes' own entries (and
/// whether the target has Environment) and of applying an outline mutation
/// and waiting for it (the app's store, or `tod-cli`'s client).
#[allow(clippy::too_many_arguments)]
pub fn transfer_with(
    creds: &CredentialStore,
    from: Uuid,
    to: Uuid,
    names: Option<&[String]>,
    mode: Mode,
    on_conflict: OnConflict,
    read: impl FnOnce() -> Result<(Vec<Entry>, Vec<Entry>, bool)>,
    mut apply: impl FnMut(crate::outline::OutlineMutation) -> Result<()>,
) -> Result<Outcome> {
    use crate::outline::OutlineMutation;
    use crate::outline::types::Capability;

    if from == to {
        bail!("the source and the target are the same node");
    }
    let (source, target, target_enabled) = read()?;
    let (merged, mut out, taken) = merge(&source, &target, names, on_conflict)?;
    if taken.is_empty() {
        return Ok(out);
    }

    // Read every secret value and check every entry first, so a failure
    // changes nothing.
    let mut values = Vec::new();
    for entry in taken.iter().filter(|e| e.kind == EntryKind::Secret) {
        values.push((entry.name.clone(), creds.get_named(&environment::secret_account(from, &entry.name))));
    }
    for entry in &taken {
        entry.validate().map_err(|e| anyhow::anyhow!("{}: {e}", entry.name))?;
    }

    for (name, value) in &values {
        let account = environment::secret_account(to, name);
        match value {
            Some(v) => {
                creds
                    .set_named(&account, v)
                    .map_err(|e| anyhow::anyhow!("could not store {name} on the target: {e}"))?;
            }
            None => {
                // The target's old value belonged to the definition being replaced.
                creds.delete_named(&account);
                out.without_value.push(name.clone());
            }
        }
    }
    if !target_enabled {
        apply(OutlineMutation::EnableCapabilities { node_id: to, capabilities: vec![Capability::Environment] })?;
    }
    apply(OutlineMutation::SetNodeEnvironment { node_id: to, entries: merged })?;

    if mode == Mode::Move {
        let mut remaining = source.clone();
        remaining.retain(|e| !taken.iter().any(|t| t.name.eq_ignore_ascii_case(&e.name)));
        apply(OutlineMutation::SetNodeEnvironment { node_id: from, entries: remaining })?;
        for entry in &taken {
            if entry.kind == EntryKind::Secret {
                environment::forget_secret(creds, from, &entry.name);
            }
            out.removed_from_source.push(entry.name.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(name: &str) -> Entry {
        let mut e = Entry::secret(name);
        e.hosts = vec!["api.example.com".into()];
        e
    }

    #[test]
    fn skip_keeps_the_targets_own_and_adds_the_rest() {
        let source = [Entry::variable("region", "eu"), secret("gb")];
        let target = [Entry::variable("REGION", "us")];
        let (merged, out, taken) = merge(&source, &target, None, OnConflict::Skip).unwrap();
        assert_eq!(out.skipped, ["region"]);
        assert_eq!(out.added, ["gb"]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].value.as_deref(), Some("us"));
        assert_eq!(taken.len(), 1);
    }

    #[test]
    fn overwrite_replaces_in_place() {
        let source = [Entry::variable("region", "eu")];
        let target = [Entry::variable("other", "1"), Entry::variable("region", "us")];
        let (merged, out, _) = merge(&source, &target, None, OnConflict::Overwrite).unwrap();
        assert_eq!(out.overwritten, ["region"]);
        assert_eq!(merged[1].value.as_deref(), Some("eu"));
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn names_limit_the_transfer_and_an_unknown_name_is_an_error() {
        let source = [Entry::variable("a", "1"), Entry::variable("b", "2")];
        let (merged, out, _) = merge(&source, &[], Some(&["B".to_string()]), OnConflict::Skip).unwrap();
        assert_eq!(out.added, ["b"]);
        assert_eq!(merged.len(), 1);
        assert!(merge(&source, &[], Some(&["nope".to_string()]), OnConflict::Skip).is_err());
    }

    #[test]
    fn summary_names_what_was_left() {
        let out = Outcome {
            added: vec!["a".into()],
            overwritten: vec!["b".into()],
            skipped: vec!["c".into()],
            ..Default::default()
        };
        assert_eq!(out.summary(Mode::Move), "2 moved (1 overwritten), 1 skipped: c");
    }
}
