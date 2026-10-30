//! Local time for agents: how the delivery header shows the time, the note after a long gap, and
//! which time zone the host runs in.

use std::path::Path;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// Gaps shorter than this get no note.
pub const GAP_NOTE_AFTER: chrono::Duration = chrono::Duration::hours(1);

/// `Thu 2026-10-30 17:05 Europe/Berlin`.
pub fn format_local(at: DateTime<Utc>, tz: Tz) -> String {
    format!("{} {}", at.with_timezone(&tz).format("%a %Y-%m-%d %H:%M"), tz.name())
}

/// `first message in 3 days` when more than [`GAP_NOTE_AFTER`] passed since `previous`.
pub fn gap_note(now: DateTime<Utc>, previous: Option<DateTime<Utc>>) -> Option<String> {
    let gap = now - previous?;
    if gap <= GAP_NOTE_AFTER {
        return None;
    }
    let (n, unit) = match gap.num_hours() {
        h if h < 48 => (h, "hour"),
        h if h < 24 * 14 => (h / 24, "day"),
        h if h < 24 * 61 => (h / (24 * 7), "week"),
        h => (h / (24 * 30), "month"),
    };
    Some(format!("first message in {n} {unit}{}", if n == 1 { "" } else { "s" }))
}

/// Where the host's time zone was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneSource {
    TzEnv,
    Localtime,
    EtcTimezone,
}

/// A zone name as `TZ`, `/etc/timezone` or a zoneinfo path spells it: `Europe/Berlin`,
/// `:Europe/Berlin`, `/usr/share/zoneinfo/Europe/Berlin`. POSIX rule strings are not names.
pub fn parse_zone(s: &str) -> Option<Tz> {
    let s = s.trim().trim_start_matches(':');
    let name = s.rsplit_once("zoneinfo/").map_or(s, |(_, n)| n);
    let name = name.strip_prefix("posix/").or_else(|| name.strip_prefix("right/")).unwrap_or(name);
    name.parse().ok()
}

/// The host's zone from, in order: the `TZ` variable, the target of the `/etc/localtime` symlink,
/// the contents of `/etc/timezone`. The first one that names a known zone wins.
pub fn host_zone(
    tz_env: Option<&str>,
    localtime_target: Option<&Path>,
    etc_timezone: Option<&str>,
) -> Option<(Tz, ZoneSource)> {
    tz_env
        .filter(|s| !s.is_empty())
        .and_then(parse_zone)
        .map(|z| (z, ZoneSource::TzEnv))
        .or_else(|| {
            localtime_target
                .and_then(|p| p.to_str())
                .filter(|p| p.contains("zoneinfo/"))
                .and_then(parse_zone)
                .map(|z| (z, ZoneSource::Localtime))
        })
        .or_else(|| etc_timezone.and_then(parse_zone).map(|z| (z, ZoneSource::EtcTimezone)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use chrono_tz::Europe::Berlin;

    #[test]
    fn formats_in_the_zone() {
        let t = Utc.with_ymd_and_hms(2026, 10, 30, 16, 5, 0).unwrap();
        assert_eq!(format_local(t, Berlin), "Fri 2026-10-30 17:05 Europe/Berlin");
        assert_eq!(format_local(t, chrono_tz::UTC), "Fri 2026-10-30 16:05 UTC");
        // Summer time.
        let s = Utc.with_ymd_and_hms(2026, 7, 2, 6, 0, 0).unwrap();
        assert_eq!(format_local(s, Berlin), "Thu 2026-07-02 08:00 Europe/Berlin");
    }

    #[test]
    fn gap_notes() {
        let now = Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap();
        let ago = |d: Duration| gap_note(now, Some(now - d));
        assert_eq!(gap_note(now, None), None);
        assert_eq!(ago(Duration::minutes(59)), None);
        assert_eq!(ago(Duration::hours(1)), None);
        assert_eq!(ago(Duration::minutes(61)).as_deref(), Some("first message in 1 hour"));
        assert_eq!(ago(Duration::hours(5)).as_deref(), Some("first message in 5 hours"));
        assert_eq!(ago(Duration::hours(47)).as_deref(), Some("first message in 47 hours"));
        assert_eq!(ago(Duration::days(3)).as_deref(), Some("first message in 3 days"));
        assert_eq!(ago(Duration::days(20)).as_deref(), Some("first message in 2 weeks"));
        assert_eq!(ago(Duration::days(400)).as_deref(), Some("first message in 13 months"));
        // A clock that went backwards is no gap.
        assert_eq!(gap_note(now, Some(now + Duration::hours(3))), None);
    }

    #[test]
    fn zone_spellings() {
        assert_eq!(parse_zone("Europe/Berlin"), Some(Berlin));
        assert_eq!(parse_zone(":Europe/Berlin\n"), Some(Berlin));
        assert_eq!(parse_zone("/usr/share/zoneinfo/Europe/Berlin"), Some(Berlin));
        assert_eq!(parse_zone("../usr/share/zoneinfo/posix/Europe/Berlin"), Some(Berlin));
        assert_eq!(parse_zone("CET-1CEST,M3.5.0,M10.5.0/3"), None);
        assert_eq!(parse_zone(""), None);
    }

    #[test]
    fn host_zone_order() {
        let link = Path::new("/usr/share/zoneinfo/America/New_York");
        let ny = chrono_tz::America::New_York;
        assert_eq!(host_zone(Some("Europe/Berlin"), Some(link), Some("Asia/Tokyo")), Some((Berlin, ZoneSource::TzEnv)));
        // An empty or POSIX-rule TZ falls through to the next source.
        assert_eq!(host_zone(Some(""), Some(link), None), Some((ny, ZoneSource::Localtime)));
        assert_eq!(host_zone(Some("CET-1CEST"), Some(link), None), Some((ny, ZoneSource::Localtime)));
        // A localtime that is not a zoneinfo link says nothing.
        assert_eq!(
            host_zone(None, Some(Path::new("/etc/localtime.real")), Some("Asia/Tokyo\n")),
            Some((chrono_tz::Asia::Tokyo, ZoneSource::EtcTimezone))
        );
        assert_eq!(host_zone(None, None, Some("Nowhere/Special")), None);
        assert_eq!(host_zone(None, None, None), None);
    }

    #[test]
    fn unset_zone_follows_the_loaders_default() {
        use crate::defs::{MemTree, TriggerKind, load_with};
        use crate::trigger::ScheduleSpec;
        let tree = |toml: &str| {
            MemTree::new()
                .with(
                    "botler.toml",
                    format!(
                        "{toml}[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"openrouter\"\nmodel = \"x\"\n"
                    ),
                )
                .with("images/agent/Dockerfile", "FROM x")
                .with(
                    "agents/botler/agent.toml",
                    "kind = \"persistent\"\n[[trigger]]\nname = \"m\"\ncron = \"0 7 * * *\"\nprompt = \"p\"\n",
                )
        };
        let zones = |toml: &str| {
            let defs = load_with(&tree(toml), Berlin).into_result().unwrap_or_else(|d| panic!("{d:#?}"));
            let TriggerKind::Schedule(ScheduleSpec::Cron(c)) = &defs.triggers.values().next().unwrap().kind else {
                panic!("not a cron trigger")
            };
            (defs.timezone, c.tz())
        };
        assert_eq!(zones(""), (Berlin, Berlin));
        assert_eq!(zones("timezone = \"Asia/Tokyo\"\n"), (chrono_tz::Asia::Tokyo, chrono_tz::Asia::Tokyo));
    }
}
