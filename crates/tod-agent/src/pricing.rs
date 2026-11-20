//! What a turn's tokens cost at Anthropic's list prices (US dollars).
//!
//! The agents report tokens, not money (Claude Code's own running cost, where
//! it writes one, covers a whole process, not a turn), so a turn's cost is its
//! recorded counts times the rate of the model that served it. It is the cost
//! at the API's list price, which is what "Claude usage" measures whatever
//! plan the agent runs under, and it is worked out when it is read, never
//! stored: a corrected rate corrects every past turn.
//!
//! Known gaps, each leaving the figure a little low or high, never silent:
//! cache writes are priced at the five-minute rate (the store keeps one
//! cache-write count, not the five-minute/one-hour split); a model the table
//! does not know is *unpriced* (`price` returns `None`) and the readers say so.

use crate::usage::{Cost, TokenCounts};

/// One model's list prices, in dollars per million tokens (which is micro-dollars per token).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing the prompt cache for five minutes (1.25 × input).
    pub cache_write: f64,
}

const fn rates(input: f64, output: f64, cache_read: f64) -> Rates {
    Rates { input, output, cache_read, cache_write: input * 1.25 }
}

/// The rates for a model as the agent names it: a full id
/// (`claude-sonnet-5-5`), or the alias Claude Code shows (`opus (For complex
/// work…)`, `sonnet`), which means the current model of that family. `None`
/// for a model the table does not know.
pub fn rates_for(model: &str) -> Option<Rates> {
    let lower = model.to_ascii_lowercase();
    // `opus (Opus 4.8)`: the word before the bracket is the alias.
    let head = lower.split(" (").next().unwrap_or(&lower).trim();
    let id = head.strip_prefix("claude-").unwrap_or(head);
    let is_alias = !id.chars().any(|c| c.is_ascii_digit());
    let has = |needles: &[&str]| needles.iter().any(|n| id.contains(n));
    if id.contains("fable") || id.contains("mythos") {
        return Some(if has(&["5-1", "5.1"]) {
            rates(10.0, 50.0, 1.0)
        } else if is_alias || has(&["5-2", "5.2"]) {
            rates(8.0, 40.0, 0.20)
        } else {
            rates(10.0, 50.0, 1.0)
        });
    }
    if id.contains("opus") {
        return Some(if is_alias || has(&["5-5", "5.5"]) {
            rates(4.0, 20.0, 0.20)
        } else if has(&["opus-5", "4-5", "4-6", "4-7", "4-8", "4.5", "4.6", "4.7", "4.8"]) {
            rates(5.0, 25.0, 0.50)
        } else {
            // Opus 4 and 4.1.
            rates(15.0, 75.0, 1.50)
        });
    }
    if id.contains("sonnet") {
        return Some(if is_alias || has(&["5-5", "5.5"]) {
            rates(2.0, 10.0, 0.10)
        } else if has(&["sonnet-5"]) {
            rates(2.0, 10.0, 0.20)
        } else {
            rates(3.0, 15.0, 0.30)
        });
    }
    if id.contains("haiku") {
        return Some(if is_alias || has(&["5-5", "5.5"]) {
            rates(0.10, 0.50, 0.01)
        } else if has(&["4-5", "4.5"]) {
            rates(1.0, 5.0, 0.10)
        } else {
            rates(0.80, 4.0, 0.08)
        });
    }
    None
}

/// What `counts` cost on `model`; `None` when the model is unknown.
pub fn price(counts: &TokenCounts, model: Option<&str>) -> Option<Cost> {
    let r = rates_for(model?)?;
    let micros = counts.input as f64 * r.input
        + counts.output as f64 * r.output
        + counts.cache_read as f64 * r.cache_read
        + counts.cache_write as f64 * r.cache_write;
    Some(Cost { micros: micros.round() as u64, currency: "USD".to_string() })
}

/// Micro-dollars as money: `$1.23`, and `<$0.01` for a spend under a cent.
pub fn format_usd(micros: u64) -> String {
    if micros == 0 {
        "$0.00".to_string()
    } else if micros < 10_000 {
        "<$0.01".to_string()
    } else {
        format!("${:.2}", micros as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_mean_the_current_model_of_the_family() {
        assert_eq!(rates_for("opus (For complex work and everyday tasks)").unwrap().input, 4.0);
        assert_eq!(rates_for("sonnet (Most efficient for simpler tasks)").unwrap().output, 10.0);
        assert_eq!(rates_for("haiku (Fastest for quick answers)").unwrap().input, 0.10);
        assert_eq!(rates_for("sonnet (Sonnet 5)").unwrap().input, 2.0);
    }

    #[test]
    fn ids_pick_their_own_generation() {
        assert_eq!(rates_for("claude-opus-5-5").unwrap().output, 20.0);
        assert_eq!(rates_for("claude-opus-4-8").unwrap().output, 25.0);
        assert_eq!(rates_for("claude-opus-4-1").unwrap().output, 75.0);
        assert_eq!(rates_for("claude-fable-5-2").unwrap().input, 8.0);
        assert_eq!(rates_for("claude-fable-5-1").unwrap().input, 10.0);
        assert_eq!(rates_for("claude-sonnet-4-6").unwrap().input, 3.0);
        assert_eq!(rates_for("claude-haiku-4-5").unwrap().output, 5.0);
        assert_eq!(rates_for("claude-sonnet-5-5").unwrap().cache_read, 0.10);
    }

    #[test]
    fn money_reads_in_dollars_and_cents() {
        assert_eq!(format_usd(0), "$0.00");
        assert_eq!(format_usd(4_000), "<$0.01");
        assert_eq!(format_usd(1_234_567), "$1.23");
    }

    #[test]
    fn an_unknown_model_is_unpriced() {
        assert_eq!(rates_for("gpt-5"), None);
        assert_eq!(price(&TokenCounts { input: 5, ..Default::default() }, None), None);
        assert_eq!(price(&TokenCounts { input: 5, ..Default::default() }, Some("auto")), None);
    }

    #[test]
    fn a_turn_costs_its_tokens_at_the_models_rates() {
        // Opus 5.5: $4 in, $20 out, $0.20 cache read, $5 cache write per million.
        let counts = TokenCounts {
            input: 1_000_000,
            output: 100_000,
            cache_read: 10_000_000,
            cache_write: 1_000_000,
            ..Default::default()
        };
        let cost = price(&counts, Some("opus")).unwrap();
        assert_eq!(cost.micros, 4_000_000 + 2_000_000 + 2_000_000 + 5_000_000);
        assert_eq!(cost.currency, "USD");
    }
}
