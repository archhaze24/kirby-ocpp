//! The clock prefix of event and frame lines (`--time utc|local|none`).

use chrono::{DateTime, FixedOffset, Local, Utc};

/// How timestamps of human-readable event lines are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeMode {
    /// `HH:MM:SS.mmm` in UTC (the default).
    #[default]
    Utc,
    /// `HH:MM:SS.mmm` in the local time zone of the machine.
    Local,
    /// No timestamp at all.
    None,
}

impl TimeMode {
    /// The clock text of `ts`, with milliseconds when `millis` is set; empty for [`Self::None`].
    #[must_use]
    pub fn clock(self, ts: DateTime<Utc>, millis: bool) -> String {
        let format = if millis { "%H:%M:%S%.3f" } else { "%H:%M:%S" };
        match self {
            Self::Utc => ts.format(format).to_string(),
            Self::Local => ts.with_timezone(&Local).format(format).to_string(),
            Self::None => String::new(),
        }
    }

    /// Like [`Self::clock`], with the time zone given explicitly instead of the machine's.
    /// `Local` uses `offset`; `Utc` and `None` ignore it.
    #[must_use]
    pub fn clock_at(self, ts: DateTime<Utc>, millis: bool, offset: FixedOffset) -> String {
        match self {
            Self::Local => {
                let format = if millis { "%H:%M:%S%.3f" } else { "%H:%M:%S" };
                ts.with_timezone(&offset).format(format).to_string()
            }
            other => other.clock(ts, millis),
        }
    }

    /// The clock with milliseconds and one trailing space, ready to put in front of a line;
    /// empty for [`Self::None`].
    #[must_use]
    pub fn prefix(self, ts: DateTime<Utc>) -> String {
        match self {
            Self::None => String::new(),
            other => format!("{} ", other.clock(ts, true)),
        }
    }

    /// The name used on the command line and in the configuration file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Utc => "utc",
            Self::Local => "local",
            Self::None => "none",
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 8, 23, 5, 9).unwrap() + chrono::Duration::milliseconds(42)
    }

    #[test]
    fn utc_is_the_default_and_keeps_the_existing_format() {
        assert_eq!(TimeMode::default(), TimeMode::Utc);
        assert_eq!(TimeMode::Utc.clock(ts(), true), "23:05:09.042");
        assert_eq!(TimeMode::Utc.clock(ts(), false), "23:05:09");
        assert_eq!(TimeMode::Utc.prefix(ts()), "23:05:09.042 ");
    }

    #[test]
    fn none_drops_the_prefix_and_its_space() {
        assert_eq!(TimeMode::None.clock(ts(), true), "");
        assert_eq!(TimeMode::None.prefix(ts()), "");
    }

    #[test]
    fn local_applies_the_offset() {
        let plus_three = FixedOffset::east_opt(3 * 3600).unwrap();
        assert_eq!(
            TimeMode::Local.clock_at(ts(), true, plus_three),
            "02:05:09.042"
        );
        let minus_half = FixedOffset::west_opt(1800).unwrap();
        assert_eq!(
            TimeMode::Local.clock_at(ts(), false, minus_half),
            "22:35:09"
        );
        assert_eq!(
            TimeMode::Utc.clock_at(ts(), true, plus_three),
            "23:05:09.042"
        );
    }

    #[test]
    fn local_uses_the_machine_zone_and_the_same_shape() {
        let text = TimeMode::Local.clock(ts(), true);
        assert_eq!(text.len(), 12, "{text}");
        assert_eq!(text.as_bytes()[8], b'.');
    }

    #[test]
    fn names_round_trip_the_flag_values() {
        assert_eq!(TimeMode::Utc.name(), "utc");
        assert_eq!(TimeMode::Local.name(), "local");
        assert_eq!(TimeMode::None.name(), "none");
    }
}
