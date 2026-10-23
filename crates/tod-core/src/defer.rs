//! Deferring a node: taking it off the user's list for a while
//! (`doc/lifecycle/next-actor.md`, "Deferring a node"). The store side is
//! `tod_store::deferrals`; the decision of whether a deferral still holds is
//! `crate::next_actor::derive`.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone};
use tod_store::fleet::{FleetStore, FleetMutation};
use uuid::Uuid;

/// The ways the user may say how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferFor {
    OneHour,
    /// 3 pm today; an hour from now when that has passed.
    ThisAfternoon,
    /// 9 am tomorrow.
    TomorrowMorning,
    Day,
    ThreeDays,
    Week,
    /// A date the user picked (ms since the epoch).
    Until(i64),
    /// Until the user brings it back.
    UntilReturned,
}

impl DeferFor {
    /// The presets a menu offers, in order.
    pub const PRESETS: [DeferFor; 7] = [
        DeferFor::OneHour,
        DeferFor::ThisAfternoon,
        DeferFor::TomorrowMorning,
        DeferFor::Day,
        DeferFor::ThreeDays,
        DeferFor::Week,
        DeferFor::UntilReturned,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            DeferFor::OneHour => "1 hour",
            DeferFor::ThisAfternoon => "This afternoon",
            DeferFor::TomorrowMorning => "Tomorrow morning",
            DeferFor::Day => "24 hours",
            DeferFor::ThreeDays => "3 days",
            DeferFor::Week => "A week",
            DeferFor::Until(_) => "Until a date",
            DeferFor::UntilReturned => "Until I bring it back",
        }
    }

    /// When the deferral ends, as ms since the epoch, counted from `now`;
    /// `None` for [`DeferFor::UntilReturned`].
    pub fn until(&self, now: DateTime<Local>) -> Option<i64> {
        let at = |time: DateTime<Local>| Some(time.timestamp_millis());
        match self {
            DeferFor::OneHour => at(now + Duration::hours(1)),
            DeferFor::ThisAfternoon => {
                let three = Local
                    .from_local_datetime(&now.date_naive().and_time(NaiveTime::from_hms_opt(15, 0, 0)?))
                    .earliest()?;
                at(if three > now { three } else { now + Duration::hours(1) })
            }
            DeferFor::TomorrowMorning => {
                let tomorrow = now.date_naive().succ_opt()?;
                at(Local
                    .from_local_datetime(&tomorrow.and_time(NaiveTime::from_hms_opt(9, 0, 0)?))
                    .earliest()?)
            }
            DeferFor::Day => at(now + Duration::hours(24)),
            DeferFor::ThreeDays => at(now + Duration::days(3)),
            DeferFor::Week => at(now + Duration::days(7)),
            DeferFor::Until(ms) => Some(*ms),
            DeferFor::UntilReturned => None,
        }
    }
}

/// Take `node` off the user's list for `how_long`.
pub fn defer(fleet: &FleetStore, node: Uuid, how_long: DeferFor) -> Result<()> {
    let now = Local::now();
    fleet
        .enqueue(FleetMutation::DeferNode {
            node,
            until: how_long.until(now),
            at: now.timestamp_millis(),
        })
        .map_err(|e| anyhow::anyhow!("could not defer the node: {e}"))
}

/// Put `node` back on the user's list.
pub fn undefer(fleet: &FleetStore, node: Uuid) -> Result<()> {
    fleet
        .enqueue(FleetMutation::UndeferNode { node })
        .map_err(|e| anyhow::anyhow!("could not bring the node back: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 23, h, m, 0).earliest().unwrap()
    }

    #[test]
    fn durations_count_from_now() {
        let now = at(10, 0);
        assert_eq!(DeferFor::OneHour.until(now), Some(at(11, 0).timestamp_millis()));
        assert_eq!(DeferFor::Day.until(now), Some((now + Duration::hours(24)).timestamp_millis()));
        assert_eq!(DeferFor::Week.until(now), Some((now + Duration::days(7)).timestamp_millis()));
        assert_eq!(DeferFor::Until(5).until(now), Some(5));
        assert_eq!(DeferFor::UntilReturned.until(now), None);
    }

    #[test]
    fn this_afternoon_is_three_or_an_hour_on_when_that_has_passed() {
        assert_eq!(DeferFor::ThisAfternoon.until(at(10, 0)), Some(at(15, 0).timestamp_millis()));
        assert_eq!(DeferFor::ThisAfternoon.until(at(16, 30)), Some(at(17, 30).timestamp_millis()));
    }

    #[test]
    fn tomorrow_morning_is_nine() {
        let tomorrow_nine = Local.with_ymd_and_hms(2026, 10, 24, 9, 0, 0).earliest().unwrap();
        assert_eq!(DeferFor::TomorrowMorning.until(at(23, 0)), Some(tomorrow_nine.timestamp_millis()));
    }

    #[test]
    fn every_preset_has_a_label_and_only_one_is_open_ended() {
        let now = at(10, 0);
        let open: Vec<_> = DeferFor::PRESETS.iter().filter(|p| p.until(now).is_none()).collect();
        assert_eq!(open, [&DeferFor::UntilReturned]);
        assert!(DeferFor::PRESETS.iter().all(|p| !p.label().is_empty()));
    }
}
