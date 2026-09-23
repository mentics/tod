//! How many tokens a session spent: what the platform's own record of it
//! says (read with its transcript), or what the agent reported over ACP
//! while a turn ran.
//!
//! Every count is what the platform said, never an estimate. A platform
//! reports what it reports: Claude Code logs each API request's usage;
//! an ACP agent may send the tokens in its context and each turn's totals.
//! Whatever is not reported stays zero or `None`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Token counts summed over some requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenCounts {
    /// Model requests counted; `0` when the platform reports totals only.
    pub requests: u64,
    /// Input tokens not read from or written to the prompt cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Of `cache_write`, what was written for five minutes and for an hour,
    /// when the platform says.
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    /// Of `output`, what went to thinking, when the platform says.
    pub thinking: u64,
    pub web_searches: u64,
    pub web_fetches: u64,
}

impl TokenCounts {
    pub fn add(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.thinking += other.thinking;
        self.web_searches += other.web_searches;
        self.web_fetches += other.web_fetches;
    }

    /// Every input token, cached or not: what the requests sent.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A cost as the platform reported it, in millionths of `currency`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    pub micros: u64,
    pub currency: String,
}

impl Cost {
    fn from_amount(amount: f64, currency: &str) -> Option<Self> {
        (amount.is_finite() && amount >= 0.0).then(|| Self {
            micros: (amount * 1_000_000.0).round() as u64,
            currency: currency.to_string(),
        })
    }

    pub fn amount(&self) -> f64 {
        self.micros as f64 / 1_000_000.0
    }
}

/// What a session spent, as far as its platform says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenUsage {
    /// Everything the session spent, its subagents' requests included.
    pub total: TokenCounts,
    /// `total`, by the model that served each request.
    pub by_model: BTreeMap<String, TokenCounts>,
    /// Of `total`, what the session's subagents spent.
    pub subagents: TokenCounts,
    /// How many tokens the session's context holds now: the whole prompt of
    /// its latest request.
    pub context_tokens: Option<u64>,
    /// The most the context can hold, when the agent says.
    pub context_window: Option<u64>,
    /// What the platform says the session cost.
    pub cost: Option<Cost>,
    /// Time spent waiting on the model, when the platform says.
    pub api_duration_ms: Option<u64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
}

impl TokenUsage {
    /// Nothing was reported at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Add another session's usage: a conversation that rotated through
    /// several. The context is the later session's.
    pub fn add(&mut self, other: &Self) {
        self.total.add(&other.total);
        self.subagents.add(&other.subagents);
        for (model, counts) in &other.by_model {
            self.by_model.entry(model.clone()).or_default().add(counts);
        }
        if other.context_tokens.is_some() {
            self.context_tokens = other.context_tokens;
        }
        if other.context_window.is_some() {
            self.context_window = other.context_window;
        }
        self.cost = match (self.cost.take(), &other.cost) {
            (Some(mut mine), Some(theirs)) if mine.currency == theirs.currency => {
                mine.micros += theirs.micros;
                Some(mine)
            }
            (mine, theirs) => mine.or_else(|| theirs.clone()),
        };
        let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        self.api_duration_ms = sum(self.api_duration_ms, other.api_duration_ms);
        self.lines_added = sum(self.lines_added, other.lines_added);
        self.lines_removed = sum(self.lines_removed, other.lines_removed);
    }

    /// Fill in what `live` (reported over ACP while the session ran) knows
    /// and this (read from the platform's record) does not.
    pub fn fill_from_live(&mut self, live: &Self) {
        if self.total.is_empty() {
            self.total = live.total.clone();
            self.by_model = live.by_model.clone();
        }
        self.context_tokens = self.context_tokens.or(live.context_tokens);
        self.context_window = self.context_window.or(live.context_window);
        if self.cost.is_none() {
            self.cost = live.cost.clone();
        }
    }
}

/// One Anthropic API `usage` object (as Claude Code logs it) as counts.
pub(crate) fn anthropic_counts(usage: &Value) -> TokenCounts {
    let n = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    TokenCounts {
        requests: 1,
        input: n("/input_tokens"),
        output: n("/output_tokens"),
        cache_read: n("/cache_read_input_tokens"),
        cache_write: n("/cache_creation_input_tokens"),
        cache_write_5m: n("/cache_creation/ephemeral_5m_input_tokens"),
        cache_write_1h: n("/cache_creation/ephemeral_1h_input_tokens"),
        thinking: n("/output_tokens_details/thinking_tokens"),
        web_searches: n("/server_tool_use/web_search_requests"),
        web_fetches: n("/server_tool_use/web_fetch_requests"),
    }
}

/// What Claude Code's `cost-state` record says: its running totals for
/// one process (`startTime`), rewritten as the process goes.
pub(crate) struct ClaudeCostState {
    pub start: u64,
    pub cost: Option<Cost>,
    pub api_duration_ms: Option<u64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
}

pub(crate) fn claude_cost_state(record: &Value) -> ClaudeCostState {
    let n = |key: &str| record.get(key).and_then(Value::as_u64);
    ClaudeCostState {
        start: n("startTime").unwrap_or(0),
        cost: record
            .get("totalCostUSD")
            .and_then(Value::as_f64)
            .and_then(|usd| Cost::from_amount(usd, "USD")),
        api_duration_ms: n("totalAPIDuration"),
        lines_added: n("totalLinesAdded"),
        lines_removed: n("totalLinesRemoved"),
    }
}

/// Apply Claude Code's cost-state records: each process's latest totals,
/// added up across processes.
pub(crate) fn apply_cost_states(usage: &mut TokenUsage, states: Vec<ClaudeCostState>) {
    let mut latest: BTreeMap<u64, ClaudeCostState> = BTreeMap::new();
    for state in states {
        // Totals only grow within a process; a record that restarts from
        // nothing is a fresh process that kept the start it resumed.
        let keep = latest.get(&state.start).is_none_or(|kept| {
            state.cost.as_ref().map_or(0, |c| c.micros) >= kept.cost.as_ref().map_or(0, |c| c.micros)
        });
        if keep {
            latest.insert(state.start, state);
        }
    }
    for state in latest.into_values() {
        usage.add(&TokenUsage {
            cost: state.cost.filter(|cost| cost.micros > 0),
            api_duration_ms: state.api_duration_ms,
            lines_added: state.lines_added,
            lines_removed: state.lines_removed,
            ..TokenUsage::default()
        });
    }
}

/// The usage an ACP agent reports while it runs, kept for one session.
#[derive(Debug, Clone, Default)]
pub(crate) struct AcpUsage {
    usage: TokenUsage,
}

impl AcpUsage {
    pub fn usage(&self) -> Option<TokenUsage> {
        (!self.usage.is_empty()).then(|| self.usage.clone())
    }

    /// A `usage_update` session update: the tokens in the context now, the
    /// context's size, and the session's cost so far.
    pub fn apply_update(&mut self, update: &Value) {
        let n = |key: &str| update.get(key).and_then(Value::as_u64);
        if let Some(used) = n("used") {
            self.usage.context_tokens = Some(used);
        }
        if let Some(size) = n("size") {
            self.usage.context_window = Some(size);
        }
        if let Some(cost) = update.get("cost") {
            let currency = cost.get("currency").and_then(Value::as_str).unwrap_or("USD");
            if let Some(cost) = cost
                .get("amount")
                .and_then(Value::as_f64)
                .and_then(|amount| Cost::from_amount(amount, currency))
            {
                self.usage.cost = Some(cost);
            }
        }
    }

    /// A `session/prompt` response's `usage`: the turn's totals.
    pub fn apply_turn(&mut self, usage: &Value) {
        let n = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let cache_read = n("cachedReadTokens");
        let cache_write = n("cachedWriteTokens");
        let mut input = n("inputTokens");
        // An agent may count the cached tokens in `inputTokens` too, which
        // its `totalTokens` then shows; keep `input` to what was neither
        // read from nor written to the cache.
        let cached = cache_read + cache_write;
        if cached > 0 && n("totalTokens") == input + n("outputTokens") && input >= cached {
            input -= cached;
        }
        self.usage.total.add(&TokenCounts {
            input,
            output: n("outputTokens"),
            cache_read,
            cache_write,
            thinking: n("thoughtTokens"),
            ..TokenCounts::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cost_states_keep_each_process_latest_and_add_processes_up() {
        let state = |start, usd: f64, api| {
            claude_cost_state(&json!({
                "type": "cost-state", "startTime": start, "totalCostUSD": usd,
                "totalAPIDuration": api, "totalLinesAdded": 1, "totalLinesRemoved": 0,
            }))
        };
        let mut usage = TokenUsage::default();
        apply_cost_states(
            &mut usage,
            vec![state(1, 0.5, 10), state(1, 1.25, 20), state(1, 0.0, 0), state(2, 0.75, 5)],
        );
        assert_eq!(usage.cost.as_ref().map(Cost::amount), Some(2.0));
        assert_eq!(usage.api_duration_ms, Some(25));
        assert_eq!(usage.lines_added, Some(2));
    }

    #[test]
    fn acp_reports_add_up_per_turn() {
        let mut acp = AcpUsage::default();
        assert_eq!(acp.usage(), None);
        acp.apply_update(&json!({ "sessionUpdate": "usage_update", "used": 900, "size": 200000,
            "cost": { "amount": 0.25, "currency": "USD" } }));
        acp.apply_turn(&json!({ "inputTokens": 100, "outputTokens": 20, "totalTokens": 120,
            "cachedReadTokens": 60, "thoughtTokens": 5 }));
        acp.apply_turn(&json!({ "inputTokens": 10, "outputTokens": 2 }));
        let usage = acp.usage().unwrap();
        assert_eq!(usage.total.input, 40 + 10);
        assert_eq!(usage.total.cache_read, 60);
        assert_eq!(usage.total.output, 22);
        assert_eq!(usage.total.thinking, 5);
        assert_eq!(usage.context_tokens, Some(900));
        assert_eq!(usage.context_window, Some(200000));
        assert_eq!(usage.cost.unwrap().micros, 250_000);
    }

    #[test]
    fn a_record_without_counts_is_filled_from_what_was_reported_live() {
        let mut live = AcpUsage::default();
        live.apply_turn(&json!({ "inputTokens": 7, "outputTokens": 3 }));
        live.apply_update(&json!({ "used": 10, "size": 100 }));
        let mut usage = TokenUsage::default();
        usage.fill_from_live(&live.usage().unwrap());
        assert_eq!(usage.total.input, 7);
        assert_eq!(usage.context_window, Some(100));
    }
}
