//! The host's time zone (the default for `botler.toml`) and how containers get the instance's.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Once, OnceLock};

use botler_core::clock::{ZoneSource, host_zone};
use botler_runtime::Mount;
use chrono_tz::Tz;

/// Where the host keeps its zone files.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// The host's time zone, detected once (see [`detect`]); UTC with a warning if none is found.
pub fn host() -> Tz {
    static HOST: OnceLock<Tz> = OnceLock::new();
    *HOST.get_or_init(|| match detect(std::env::var("TZ").ok().as_deref(), Path::new("/")) {
        Some((tz, source)) => {
            tracing::info!(timezone = tz.name(), ?source, "host time zone");
            tz
        }
        None => {
            tracing::warn!("could not tell the host's time zone (TZ, /etc/localtime, /etc/timezone); using UTC");
            Tz::UTC
        }
    })
}

/// The zone of the system rooted at `root`: `TZ`, then the `/etc/localtime` symlink target, then
/// `/etc/timezone`.
pub fn detect(tz_env: Option<&str>, root: &Path) -> Option<(Tz, ZoneSource)> {
    let link = std::fs::read_link(root.join("etc/localtime")).ok();
    let file = std::fs::read_to_string(root.join("etc/timezone")).ok();
    host_zone(tz_env, link.as_deref(), file.as_deref())
}

/// Make a container run in `tz`: `TZ`, plus the host's zone file at `/etc/localtime` and at its
/// zoneinfo path, so images without tzdata (where `TZ` alone falls back to UTC) agree.
pub fn apply(tz: Tz, zoneinfo: &Path, env: &mut BTreeMap<String, String>, mounts: &mut Vec<Mount>) {
    env.insert("TZ".into(), tz.name().into());
    let file = zoneinfo.join(tz.name());
    if file.is_file() {
        for dst in ["/etc/localtime".to_string(), format!("{ZONEINFO}/{}", tz.name())] {
            mounts.push(Mount::Bind { src: file.clone(), dst, read_only: true });
        }
    } else {
        static WARN: Once = Once::new();
        WARN.call_once(|| {
            tracing::warn!(file = %file.display(), "no host zone file; containers without tzdata will run in UTC");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_from_a_fake_root() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        assert_eq!(detect(None, root.path()), None);
        std::fs::write(etc.join("timezone"), "Asia/Tokyo\n").unwrap();
        assert_eq!(detect(None, root.path()), Some((chrono_tz::Asia::Tokyo, ZoneSource::EtcTimezone)));
        std::os::unix::fs::symlink("../usr/share/zoneinfo/America/New_York", etc.join("localtime")).unwrap();
        assert_eq!(detect(None, root.path()), Some((chrono_tz::America::New_York, ZoneSource::Localtime)));
        assert_eq!(detect(Some(":Europe/Berlin"), root.path()), Some((chrono_tz::Europe::Berlin, ZoneSource::TzEnv)));
    }

    #[test]
    fn containers_get_tz_and_the_zone_file() {
        let zi = tempfile::tempdir().unwrap();
        std::fs::create_dir(zi.path().join("Europe")).unwrap();
        std::fs::write(zi.path().join("Europe/Berlin"), "TZif").unwrap();
        let (mut env, mut mounts) = (BTreeMap::new(), Vec::new());
        apply(chrono_tz::Europe::Berlin, zi.path(), &mut env, &mut mounts);
        assert_eq!(env["TZ"], "Europe/Berlin");
        let src = zi.path().join("Europe/Berlin");
        assert_eq!(
            mounts,
            vec![
                Mount::Bind { src: src.clone(), dst: "/etc/localtime".into(), read_only: true },
                Mount::Bind { src, dst: "/usr/share/zoneinfo/Europe/Berlin".into(), read_only: true },
            ]
        );
        // No zone file on the host: TZ only.
        let (mut env, mut mounts) = (BTreeMap::new(), Vec::new());
        apply(chrono_tz::Asia::Tokyo, zi.path(), &mut env, &mut mounts);
        assert_eq!(env["TZ"], "Asia/Tokyo");
        assert!(mounts.is_empty());
    }
}
