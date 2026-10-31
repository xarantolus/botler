//! Human-readable durations (`"15m"`, `"1h 30m"`) for definition files.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration written as a human-readable string in definition files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HumanDuration(pub Duration);

impl HumanDuration {
    pub const fn from_secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }

    pub fn as_chrono(self) -> chrono::Duration {
        chrono::Duration::from_std(self.0).unwrap_or(chrono::Duration::MAX)
    }
}

impl FromStr for HumanDuration {
    type Err = humantime::DurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        humantime::parse_duration(s).map(Self)
    }
}

/// Compact, as people write them in botler.toml: `30d`, `1h30m`, `90s` (sub-second rest in
/// humantime's form).
impl fmt::Display for HumanDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.subsec_nanos() != 0 {
            return write!(f, "{}", humantime::format_duration(self.0));
        }
        let mut rest = self.0.as_secs();
        if rest == 0 {
            return f.write_str("0s");
        }
        for (unit, secs) in [("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)] {
            if rest >= secs {
                write!(f, "{}{unit}", rest / secs)?;
                rest %= secs;
            }
        }
        Ok(())
    }
}

impl Serialize for HumanDuration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for HumanDuration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HumanDuration".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Human-readable duration, e.g. \"30s\", \"15m\", \"1h 30m\"."
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_display_round_trips() {
        for (text, shown) in [("30d", "30d"), ("90m", "1h30m"), ("900s", "15m"), ("0s", "0s"), ("1day 2h", "1d2h")] {
            let d: super::HumanDuration = text.parse().unwrap();
            assert_eq!(d.to_string(), shown);
            assert_eq!(shown.parse::<super::HumanDuration>().unwrap(), d);
        }
    }

    use super::*;

    #[test]
    fn parses_and_roundtrips() {
        let d: HumanDuration = "1h 30m".parse().unwrap();
        assert_eq!(d.0, Duration::from_secs(5400));
        let toml = toml::to_string(&std::collections::BTreeMap::from([("t", d)])).unwrap();
        let back: std::collections::BTreeMap<String, HumanDuration> = toml::from_str(&toml).unwrap();
        assert_eq!(back["t"], d);
    }
}
