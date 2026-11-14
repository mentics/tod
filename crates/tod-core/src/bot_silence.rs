//! A review bot that stays silent: the one request the user gets for it, and
//! what answering it does. Spec: `doc/lifecycle/pr-readiness.md`, "A bot that
//! stays silent".
//!
//! A bot that has not reviewed the current head is waited for, and asked by
//! tod (`pr_readiness::Assessment::asks_due`), a bounded number of times. When
//! it is asked as often as allowed and still has not come
//! (`Assessment::silent_bots`), nothing the node can do will bring it: it may
//! be down or out of quota. The user is asked once, as a pending decision on
//! the node (protocol [`PROTOCOL`], reason `broken`) with fixed wording that
//! lists what was tried, and two options: proceed without the bot, or keep
//! waiting.
//!
//! "Proceed without" is a standing answer on the node: the bot is left out of
//! the settings every reader of the pull request uses
//! ([`settings_for_node`]), so the gate's `review-current` and `review-score`
//! criteria, the waits, and the asks all stop counting it, for this node.
//! "Keep waiting" holds the question back for [`KEEP_WAITING_HOURS`].

use chrono::{DateTime, Utc};
use tod_store::decisions::{Decision, DecisionRepo, NewDecision, REASON_BROKEN};
use tod_store::settings::{OwnReviewState, PrReadinessSettings};
use uuid::Uuid;

/// The `protocol` the request is recorded with.
pub const PROTOCOL: &str = "bot_silence";

const PROCEED_PREFIX: &str = "Proceed without ";
/// The option that keeps waiting.
pub const KEEP_WAITING: &str = "Keep waiting";

/// After "Keep waiting", how long before the user is asked again.
pub const KEEP_WAITING_HOURS: i64 = 6;

/// What was tried, for the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tried {
    pub bot: String,
    /// The pull requests it has not reviewed, as `owner/repo#n`.
    pub prs: Vec<String>,
    pub asks: u32,
    pub last_ask_at: Option<DateTime<Utc>>,
    /// When the bot last reviewed or scored a pull request of the node.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// The bot said it will not review (its `exhausted_pattern` matched its
    /// latest comment): no waiting would bring it.
    pub exhausted: bool,
}

fn when(at: Option<DateTime<Utc>>, none: &str) -> String {
    at.map_or_else(|| none.to_string(), |t| t.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// The option that goes on without `bot`.
pub fn proceed_option(bot: &str) -> String {
    format!("{PROCEED_PREFIX}{bot}")
}

/// The request's question: one wording for every node and bot.
pub fn question(tried: &Tried) -> String {
    if tried.exhausted {
        return format!(
            "{bot} said it will not review {prs} (its latest comment matches the project's out-of-reviews pattern; asked {asks} time{plural}, last {asked}). Waiting will not bring it. Proceed without {bot}, or keep waiting?",
            bot = tried.bot,
            prs = tried.prs.join(", "),
            asks = tried.asks,
            plural = if tried.asks == 1 { "" } else { "s" },
            asked = when(tried.last_ask_at, "never"),
        );
    }
    format!(
        "{bot} has not reviewed {prs} after {asks} ask{plural} for a review (last asked {asked}; its last activity {seen}). It may be down or out of quota, which this node cannot fix. Proceed without {bot}, or keep waiting?",
        bot = tried.bot,
        prs = tried.prs.join(", "),
        asks = tried.asks,
        plural = if tried.asks == 1 { "" } else { "s" },
        asked = when(tried.last_ask_at, "never"),
        seen = when(tried.last_activity_at, "none seen"),
    )
}

/// The options, in order.
pub fn options(bot: &str) -> Vec<String> {
    vec![proceed_option(bot), KEEP_WAITING.to_string()]
}

/// The bot a bot-silence decision is about.
fn bot_of(decision: &Decision) -> Option<&str> {
    if decision.protocol.as_deref() != Some(PROTOCOL) {
        return None;
    }
    decision.options.first()?.strip_prefix(PROCEED_PREFIX)
}

/// What the node's saved answers say about `bot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Never raised, or raised and held back no longer.
    Unasked,
    /// Raised and not answered.
    Pending,
    /// The user said to go on without it.
    Waived,
    /// The user said to keep waiting, at this time (ms).
    KeepWaiting { at_ms: i64 },
}

/// `bot`'s standing on `node`, from its bot-silence decisions.
pub fn standing(conn: &rusqlite::Connection, node: Uuid, bot: &str) -> anyhow::Result<Standing> {
    let repo = DecisionRepo::new(conn);
    let mut keep: Option<i64> = None;
    for decision in repo.list_for_node(node)? {
        if !bot_of(&decision).is_some_and(|b| b.eq_ignore_ascii_case(bot)) {
            continue;
        }
        match decision.status.as_str() {
            "pending" => return Ok(Standing::Pending),
            "answered" => {
                let Some(found) = repo.get_with_answers(decision.id)? else { continue };
                let Some(answer) = found.answers.last() else { continue };
                match answer.option {
                    Some(1) => return Ok(Standing::Waived),
                    Some(_) => keep = keep.max(Some(answer.answered_at)),
                    None => {}
                }
            }
            _ => {}
        }
    }
    Ok(keep.map_or(Standing::Unasked, |at_ms| Standing::KeepWaiting { at_ms }))
}

/// Whether the user is to be asked about `bot` now.
pub fn should_ask(standing: Standing, now: DateTime<Utc>) -> bool {
    match standing {
        Standing::Unasked => true,
        Standing::Pending | Standing::Waived => false,
        Standing::KeepWaiting { at_ms } => {
            now.timestamp_millis() - at_ms >= KEEP_WAITING_HOURS * 3_600_000
        }
    }
}

/// The names of the bots the user told `node` to go on without.
pub fn waived(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<String>> {
    let repo = DecisionRepo::new(conn);
    let mut out: Vec<String> = Vec::new();
    for decision in repo.list_for_node(node)? {
        let Some(bot) = bot_of(&decision).map(str::to_string) else { continue };
        if decision.status != "answered" {
            continue;
        }
        let Some(found) = repo.get_with_answers(decision.id)? else { continue };
        if found.answers.last().and_then(|a| a.option) == Some(1) && !out.iter().any(|b| b.eq_ignore_ascii_case(&bot)) {
            out.push(bot);
        }
    }
    Ok(out)
}

/// `settings` without the bots `waived` names: what every reader of `node`'s
/// pull requests uses, so a bot the user said to go on without is not
/// waited for, asked, or required by a criterion.
pub fn without_waived(settings: &PrReadinessSettings, waived: &[String]) -> PrReadinessSettings {
    let mut settings = settings.clone();
    let (gone, kept): (Vec<_>, Vec<_>) = settings
        .bots
        .drain(..)
        .partition(|b| waived.iter().any(|w| w.eq_ignore_ascii_case(&b.name)));
    settings.bots = kept;
    // Its asks stay on the pull request, and are still not feedback.
    settings.waived_asks.extend(
        gone.iter().filter_map(crate::pr_readiness::ask_text).map(|a| a.to_ascii_lowercase()),
    );
    settings
}

/// [`without_waived`] for `node`, read from its decisions.
pub fn settings_for_node(
    conn: &rusqlite::Connection,
    node: Uuid,
    settings: PrReadinessSettings,
) -> anyhow::Result<PrReadinessSettings> {
    let waived = waived(conn, node)?;
    let mut settings = if waived.is_empty() { settings } else { without_waived(&settings, &waived) };
    // Where tod's own review stands: a bot is asked for a head only once it
    // is clean, and a bot's `fallback` relies on it. Per head: the cleared
    // heads are the ones the latest clean review covered, so a later push
    // starts again. Nothing is read for a project with no bots.
    if !settings.bots.is_empty() {
        settings.own_review = own_review_state(conn, node)?;
        // Every repository tod has pull requests in: where a bot's history
        // is read from when finding out whether it reviews drafts.
        let mut repos: Vec<(String, String)> = Vec::new();
        for prs in tod_store::github::NodePrRepo::new(conn).all()?.into_values() {
            for pr in prs {
                let key = (pr.owner.clone(), pr.repo.clone());
                if !repos.iter().any(|(o, r)| o.eq_ignore_ascii_case(&key.0) && r.eq_ignore_ascii_case(&key.1)) {
                    repos.push(key);
                }
            }
        }
        repos.sort();
        settings.known_repos = repos;
    }
    Ok(settings)
}

/// [`OwnReviewState`] of `node`.
pub fn own_review_state(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<OwnReviewState> {
    let enabled = crate::pr_draft_review::enabled(conn, node)?;
    if !enabled {
        return Ok(OwnReviewState::default());
    }
    let state = tod_store::outline::repos::NodeRepo::new(conn).get_lifecycle(node)?;
    let in_draft = state.as_deref() == Some(crate::pr_draft_review::STATE);
    let in_review = state.as_deref() == Some("pr_review");
    Ok(OwnReviewState { enabled, in_draft, in_review, cleared_shas: crate::pr_draft_review::cleared_shas(conn, node, in_review)? })
}

/// The decision to record for `tried`.
pub fn new_decision(tried: &Tried) -> NewDecision {
    NewDecision {
        question: question(tried),
        options: options(&tried.bot),
        evidence: Vec::new(),
        reason: REASON_BROKEN.to_string(),
        moves: Vec::new(),
        ..Default::default()
    }
}

/// Record that the user told `node` to go on without `bot`.
#[cfg(test)]
pub(crate) fn waive_for_test(fleet: &tod_store::fleet::FleetStore, node: Uuid, bot: &str) {
    use tod_store::interview::{ACTOR_USER, InterviewCommand};
    let tried = Tried { bot: bot.into(), prs: vec!["o/r#1".into()], asks: 3, last_ask_at: None, last_activity_at: None, exhausted: false };
    fleet
        .interview(
            "autopilot",
            InterviewCommand::AskDecision {
                node_id: node,
                conversation_id: None,
                protocol: Some(PROTOCOL.to_string()),
                decision: new_decision(&tried),
            },
        )
        .unwrap();
    let id = fleet.read(|conn| Ok(DecisionRepo::new(conn).list_for_node(node)?)).unwrap().last().unwrap().id;
    fleet
        .interview(ACTOR_USER, InterviewCommand::AnswerDecision { decision_id: id, option: Some(1), text: None })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tried() -> Tried {
        Tried {
            bot: "greptile".into(),
            prs: vec!["o/r#1".into()],
            asks: 3,
            last_ask_at: DateTime::parse_from_rfc3339("2026-11-12T10:00:00Z").ok().map(Into::into),
            last_activity_at: None,
            exhausted: false,
        }
    }

    #[test]
    fn the_wording_is_fixed_and_lists_what_was_tried() {
        assert_eq!(
            question(&tried()),
            "greptile has not reviewed o/r#1 after 3 asks for a review (last asked 2026-11-12 10:00 UTC; its last activity none seen). It may be down or out of quota, which this node cannot fix. Proceed without greptile, or keep waiting?"
        );
        assert_eq!(options("greptile"), vec!["Proceed without greptile", "Keep waiting"]);
    }

    #[test]
    fn a_waived_bot_is_left_out_of_the_settings() {
        let settings = PrReadinessSettings {
            bots: vec![
                tod_store::settings::PrReviewBotSettings::new("greptile", 4, 10),
                tod_store::settings::PrReviewBotSettings::new("other", 3, 10),
            ],
            ..Default::default()
        };
        let kept = without_waived(&settings, &["Greptile".to_string()]);
        assert_eq!(kept.bots.len(), 1);
        assert_eq!(kept.bots[0].name, "other");
    }

    #[test]
    fn keeping_waiting_holds_the_question_back_for_hours() {
        let now = Utc::now();
        let at_ms = now.timestamp_millis() - 3_600_000;
        assert!(!should_ask(Standing::KeepWaiting { at_ms }, now));
        assert!(should_ask(Standing::KeepWaiting { at_ms: at_ms - KEEP_WAITING_HOURS * 3_600_000 }, now));
        assert!(should_ask(Standing::Unasked, now));
        assert!(!should_ask(Standing::Pending, now));
        assert!(!should_ask(Standing::Waived, now));
    }
}
