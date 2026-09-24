//! A session's token usage as the transcripts show it: one line to glance
//! at, and every figure the platform reported for whoever wants them.

use tod_agent::{Cost, TokenCounts, TokenUsage};

/// A token count, compact: `950`, `12.3k`, `1.24M`.
pub fn compact(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => trimmed(count as f64 / 1_000.0, "k"),
        _ => trimmed(count as f64 / 1_000_000.0, "M"),
    }
}

/// `value` with three significant figures at most, and no trailing zeros.
fn trimmed(value: f64, unit: &str) -> String {
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    let text = format!("{value:.decimals$}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    format!("{text}{unit}")
}

fn cost(cost: &Cost) -> String {
    match cost.currency.as_str() {
        "USD" => format!("${:.2}", cost.amount()),
        currency => format!("{:.2} {currency}", cost.amount()),
    }
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

fn context(usage: &TokenUsage) -> Option<String> {
    match (usage.context_tokens, usage.context_window) {
        (Some(used), Some(size)) if size > 0 => Some(format!(
            "{} / {} ({}%)",
            compact(used),
            compact(size),
            used * 100 / size
        )),
        (Some(used), _) => Some(compact(used)),
        (None, Some(size)) => Some(format!("? / {}", compact(size))),
        (None, None) => None,
    }
}

fn requests(count: u64) -> String {
    match count {
        1 => "1 request".to_string(),
        n => format!("{n} requests"),
    }
}

/// The counts in a line: input, output, the cache, then the requests.
fn counts_line(counts: &TokenCounts) -> String {
    let mut parts = vec![
        format!("{} in", compact(counts.input)),
        format!("{} out", compact(counts.output)),
    ];
    if counts.cache_read > 0 {
        parts.push(format!("{} cache read", compact(counts.cache_read)));
    }
    if counts.cache_write > 0 {
        parts.push(format!("{} cache write", compact(counts.cache_write)));
    }
    if counts.requests > 0 {
        parts.push(requests(counts.requests));
    }
    parts.join(" · ")
}

/// One line: the totals, the context, and the cost.
pub fn summary(usage: &TokenUsage) -> String {
    let mut line = format!("Tokens: {}", counts_line(&usage.total));
    if let Some(context) = context(usage) {
        line.push_str(&format!(" · context {context}"));
    }
    if let Some(amount) = &usage.cost {
        line.push_str(&format!(" · {}", cost(amount)));
    }
    line
}

/// Every figure, a line each.
pub fn details(usage: &TokenUsage) -> String {
    let total = &usage.total;
    let mut lines = vec![
        format!("Input (uncached): {}", total.input),
        format!(
            "Output: {}{}",
            total.output,
            if total.thinking > 0 {
                format!(" (thinking {})", total.thinking)
            } else {
                String::new()
            }
        ),
        format!("Cache read: {}", total.cache_read),
    ];
    let mut write = format!("Cache write: {}", total.cache_write);
    if total.cache_write_5m > 0 || total.cache_write_1h > 0 {
        write.push_str(&format!(
            " (5m {}, 1h {})",
            total.cache_write_5m, total.cache_write_1h
        ));
    }
    lines.push(write);
    lines.push(format!("Total input: {}", total.prompt()));
    if total.requests > 0 {
        lines.push(format!("Requests: {}", total.requests));
    }
    if total.web_searches > 0 || total.web_fetches > 0 {
        lines.push(format!(
            "Web searches: {} · web fetches: {}",
            total.web_searches, total.web_fetches
        ));
    }
    if let Some(context) = context(usage) {
        lines.push(format!("Context now: {context}"));
    }
    if let Some(amount) = &usage.cost {
        lines.push(format!("Cost: {}", cost(amount)));
    }
    if let Some(ms) = usage.api_duration_ms {
        lines.push(format!("Time waiting on the model: {}", duration(ms)));
    }
    if usage.lines_added.is_some() || usage.lines_removed.is_some() {
        lines.push(format!(
            "Lines: +{} −{}",
            usage.lines_added.unwrap_or(0),
            usage.lines_removed.unwrap_or(0)
        ));
    }
    if !usage.subagents.is_empty() {
        lines.push(format!("Of which subagents: {}", counts_line(&usage.subagents)));
    }
    if usage.by_model.len() > 1 {
        for (model, counts) in &usage.by_model {
            lines.push(format!("{model}: {}", counts_line(counts)));
        }
    } else if let Some(model) = usage.by_model.keys().next() {
        lines.push(format!("Model: {model}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_compactly() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(1_000), "1k");
        assert_eq!(compact(12_345), "12.3k");
        assert_eq!(compact(108_947), "109k");
        assert_eq!(compact(1_240_000), "1.24M");
    }

    #[test]
    fn the_summary_leaves_out_what_was_not_reported() {
        let mut usage = TokenUsage::default();
        usage.total.input = 1200;
        usage.total.output = 30;
        assert_eq!(summary(&usage), "Tokens: 1.2k in · 30 out");
        usage.total.cache_read = 5_000;
        usage.total.requests = 2;
        usage.context_tokens = Some(50_000);
        usage.context_window = Some(200_000);
        usage.cost = Some(Cost {
            micros: 1_234_567,
            currency: "USD".into(),
        });
        assert_eq!(
            summary(&usage),
            "Tokens: 1.2k in · 30 out · 5k cache read · 2 requests · context 50k / 200k (25%) · $1.23"
        );
    }
}
