use crate::interview::settings::ResearcherSettings;

/// How many new researcher runs to start given current queue depth and in-flight count.
pub fn researcher_starts_needed(
    open_count: usize,
    in_flight_researcher: usize,
    settings: &ResearcherSettings,
) -> u32 {
    if in_flight_researcher >= 2 {
        return 0;
    }
    let mut to_start = 0u32;
    if open_count < settings.replenish_threshold as usize
        && in_flight_researcher + (to_start as usize) < 2
    {
        to_start += 1;
    }
    if open_count < settings.second_researcher_threshold as usize
        && in_flight_researcher >= 1
        && in_flight_researcher + (to_start as usize) < 2
    {
        to_start += 1;
    }
    to_start
}

/// Exponential backoff delay in seconds for researcher retry (1, 2, 4).
pub fn retry_backoff_secs(attempt: u32) -> u64 {
    1u64 << attempt.min(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::settings::ResearcherSettings;

    fn settings() -> ResearcherSettings {
        ResearcherSettings {
            replenish_threshold: 8,
            second_researcher_threshold: 2,
        }
    }

    #[test]
    fn no_starts_when_queue_full() {
        assert_eq!(researcher_starts_needed(10, 0, &settings()), 0);
    }

    #[test]
    fn starts_one_below_replenish_threshold() {
        assert_eq!(researcher_starts_needed(7, 0, &settings()), 1);
    }

    #[test]
    fn starts_second_when_below_second_threshold_and_one_in_flight() {
        assert_eq!(researcher_starts_needed(1, 1, &settings()), 1);
    }

    #[test]
    fn caps_at_two_concurrent() {
        assert_eq!(researcher_starts_needed(0, 2, &settings()), 0);
        assert_eq!(researcher_starts_needed(0, 1, &settings()), 1);
    }

    #[test]
    fn backoff_doubles_up_to_four_seconds() {
        assert_eq!(retry_backoff_secs(0), 1);
        assert_eq!(retry_backoff_secs(1), 2);
        assert_eq!(retry_backoff_secs(2), 4);
        assert_eq!(retry_backoff_secs(5), 4);
    }
}
