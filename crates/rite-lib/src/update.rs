//! Finding out that a newer release exists, and remembering "Later".
//!
//! This is only the *detecting* half of in-app updates (ticket 190): it
//! reports `UpdateAvailable` and keeps the snooze. Applying an update is
//! ticket 191, the banner ticket 192.
//!
//! The library has no HTTP client, so asking GitHub is a `ReleaseSource` the
//! caller supplies (the front ends use `ureq`) and the tests replace with a
//! fake. Everything here is silent about failure: an offline machine, a rate
//! limit or a garbled answer means "nothing to report", never an error the
//! user has to read.
//!
//! The state is machine-local, in `$XDG_STATE_HOME/kanrite/update.json`,
//! never in a tickets or documents repository: whether *you* have seen a
//! release is nothing a teammate's clone should inherit.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::settings::{self, Config};

/// How long a result is trusted before GitHub is asked again. Three hours,
/// so a release shows the same day (ticket 285: a day, with the bar's own six
/// hours on top, let 4.8.0 go unseen for over a day); still a handful of
/// requests a day against the releases API's 60 an hour.
pub const CHECK_EVERY: i64 = 3 * 60 * 60;
/// How long "Later" hides a release.
pub const SNOOZE_FOR: i64 = 24 * 60 * 60;
/// How soon to try again after a failed check, so an offline laptop doesn't
/// ask on every call but doesn't wait a whole day either.
pub const RETRY_AFTER_FAILURE: i64 = 60 * 60;
/// A check asked for by hand (ticket 304) goes to GitHub at most this often;
/// clicks within it answer from what the last one found.
pub const MANUAL_EVERY: i64 = 60;

/// `major.minor.patch`. A pre-release (`4.1.0-rc1`) does not parse, which is
/// how pre-releases are kept out of the offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `4.1.0` or `v4.1.0`.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let version = Version(
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// One published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub notes_url: String,
    /// What the release says, as GitHub has it (its markdown body; ticket
    /// 336); empty in the remembered list and where it wrote none.
    pub notes: String,
    /// Its downloadable files (ticket 191); empty where only the version
    /// matters, such as the remembered list.
    pub assets: Vec<Asset>,
}

/// One file attached to a release: its name, where to download it, and the
/// SHA-256 GitHub publishes for it (lowercase hex; empty if it gave none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

/// Where releases come from. The caller supplies the network.
pub trait ReleaseSource {
    /// Every published release, in any order. An `Err` is a failed check.
    fn releases(&self) -> Result<Vec<Release>>;
}

/// What the banner shows: the newest release, and how many releases the build
/// is behind (so an update can catch up one release at a time, ticket 191).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAvailable {
    pub version: String,
    pub notes_url: String,
    pub behind: usize,
}

/// Reads GitHub's `GET /repos/{owner}/{repo}/releases` answer. Drafts,
/// pre-releases and tags that aren't `major.minor.patch` are left out; so is
/// anything it can't read, rather than failing the lot.
pub fn parse_releases(json: &str) -> Result<Vec<Release>> {
    #[derive(Deserialize)]
    struct Item {
        tag_name: String,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
        /// GitHub sends `null` for a release with no notes.
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        assets: Vec<File>,
    }
    #[derive(Deserialize)]
    struct File {
        name: String,
        #[serde(default)]
        browser_download_url: String,
        /// `sha256:<hex>`, which GitHub gives every release file.
        #[serde(default)]
        digest: Option<String>,
    }
    let items: Vec<Item> = serde_json::from_str(json)?;
    Ok(items
        .into_iter()
        .filter(|item| !item.draft && !item.prerelease)
        .filter_map(|item| {
            Some(Release {
                version: Version::parse(&item.tag_name)?,
                notes_url: item.html_url,
                notes: item.body.unwrap_or_default(),
                assets: item
                    .assets
                    .into_iter()
                    .map(|file| Asset {
                        name: file.name,
                        url: file.browser_download_url,
                        sha256: file
                            .digest
                            .as_deref()
                            .and_then(|d| d.strip_prefix("sha256:"))
                            .unwrap_or_default()
                            .to_lowercase(),
                    })
                    .collect(),
            })
        })
        .collect())
}

/// The newest release above `current`, and how many are above it.
pub fn newest_above(current: Version, releases: &[Release]) -> Option<UpdateAvailable> {
    let newer: Vec<&Release> = releases.iter().filter(|r| r.version > current).collect();
    let latest = newer.iter().max_by_key(|r| r.version)?;
    Some(UpdateAvailable {
        version: latest.version.to_string(),
        notes_url: latest.notes_url.clone(),
        behind: newer.len(),
    })
}

/// What's new (ticket 336): every release above `current`, newest first.
pub fn newer_than(current: Version, releases: &[Release]) -> Vec<&Release> {
    let mut newer: Vec<&Release> = releases.iter().filter(|r| r.version > current).collect();
    newer.sort_by_key(|r| std::cmp::Reverse(r.version));
    newer
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Not before this (unix seconds) is GitHub asked again.
    #[serde(default)]
    next_check: i64,
    /// The newest release seen, whatever this build was when it saw it.
    #[serde(default)]
    releases: Vec<(String, String)>,
    #[serde(default)]
    snoozed: Option<Snooze>,
    /// When GitHub last answered (unix seconds; 0 unknown, as a file from
    /// before ticket 304 has).
    #[serde(default)]
    checked: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Snooze {
    version: String,
    until: i64,
}

impl State {
    /// A missing or unreadable file is a fresh state, never an error.
    fn load(path: &Path) -> State {
        crate::eol::read(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Temp file then rename, so the CLI and riteui can't leave it half-written.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".tmp.{}", std::process::id()));
        let tmp = path.with_file_name(tmp_name);
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn known(&self) -> Vec<Release> {
        self.releases
            .iter()
            .filter_map(|(tag, url)| {
                Some(Release {
                    version: Version::parse(tag)?,
                    notes_url: url.clone(),
                    notes: String::new(),
                    assets: Vec::new(),
                })
            })
            .collect()
    }
}

/// `update.json` in the state folder ([`crate::places::state`]).
pub fn state_path() -> Option<PathBuf> {
    crate::places::state().map(|state| state.join("update.json"))
}

/// Whether to look for updates at all: `RITE_UPDATE_CHECK`, then the
/// `update-check` key in the global config, then **on**. `0`, `off`, `no`
/// and `false` switch it off. The check makes a network call the user didn't
/// ask for, so it can be turned off.
pub fn enabled(config: &Config) -> bool {
    settings::resolved("RITE_UPDATE_CHECK", config, "update-check")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "no" | "false"
            )
        })
        .unwrap_or(true)
}

/// What, if anything, to tell the user about.
///
/// Asks `source` only when the last answer is `CHECK_EVERY` old (or an hour
/// old after a failure); otherwise answers from the remembered list. `None` when this
/// build is current, when the release was snoozed and the day isn't up, or
/// when nothing is known (offline on first run).
pub fn check(
    source: &dyn ReleaseSource,
    current: Version,
    now: i64,
    state_file: &Path,
) -> Option<UpdateAvailable> {
    let mut state = State::load(state_file);

    // A clock set back would otherwise hold the check off for ages.
    let due = now >= state.next_check || state.next_check - now > CHECK_EVERY;
    if due {
        match source.releases() {
            Ok(releases) => {
                state.releases = releases
                    .iter()
                    .map(|r| (r.version.to_string(), r.notes_url.clone()))
                    .collect();
                state.next_check = now + CHECK_EVERY;
                state.checked = now;
            }
            Err(_) => state.next_check = now + RETRY_AFTER_FAILURE,
        }
        // Remembering is a courtesy; an unwritable state dir must not stop
        // the answer.
        let _ = state.save(state_file);
    }

    let available = newest_above(current, &state.known())?;
    // Versions are always written by `Version`'s Display, so equal versions
    // are equal text.
    let snoozed = state
        .snoozed
        .as_ref()
        .is_some_and(|snooze| snooze.version == available.version && now < snooze.until);
    (!snoozed).then_some(available)
}

/// "Later": hides `version` for a day. A newer release than this one is still
/// shown straight away, because the snooze is for the version, not for the
/// banner.
/// What a check asked for by hand found (ticket 304).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manual {
    /// A newer release.
    Newer(UpdateAvailable),
    /// This build is the newest.
    Current,
    /// GitHub couldn't be asked; why.
    Failed(String),
}

/// A check asked for by hand (ticket 304): asks `source` now, whatever
/// `CHECK_EVERY` says and whether or not automatic checks are on (asking is
/// consent), but at most once every [`MANUAL_EVERY`]; a quicker click answers
/// from that last answer. It clears a Later, and what it finds is remembered
/// as an automatic check's would be.
pub fn check_now(
    source: &dyn ReleaseSource,
    current: Version,
    now: i64,
    state_file: &Path,
) -> Manual {
    let mut state = State::load(state_file);
    state.snoozed = None;
    let recent = state.checked > 0 && (0..MANUAL_EVERY).contains(&(now - state.checked));
    if !recent {
        match source.releases() {
            Ok(releases) => {
                state.releases = releases
                    .iter()
                    .map(|r| (r.version.to_string(), r.notes_url.clone()))
                    .collect();
                state.next_check = now + CHECK_EVERY;
                state.checked = now;
            }
            Err(err) => {
                let _ = state.save(state_file);
                return Manual::Failed(format!("{err:#}"));
            }
        }
    }
    let _ = state.save(state_file);
    match newest_above(current, &state.known()) {
        Some(found) => Manual::Newer(found),
        None => Manual::Current,
    }
}

/// When GitHub last answered a check, if it's known.
pub fn last_checked(state_file: &Path) -> Option<i64> {
    Some(State::load(state_file).checked).filter(|c| *c > 0)
}

pub fn snooze(state_file: &Path, version: &str, now: i64) -> Result<()> {
    let mut state = State::load(state_file);
    state.snoozed = Some(Snooze {
        version: version.to_string(),
        until: now + SNOOZE_FOR,
    });
    state.save(state_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fake {
        releases: Vec<Release>,
        fail: Cell<bool>,
        asked: Cell<u32>,
    }

    impl Fake {
        fn with(versions: &[&str]) -> Fake {
            Fake {
                releases: versions.iter().map(|v| release(v)).collect(),
                fail: Cell::new(false),
                asked: Cell::new(0),
            }
        }
    }

    impl ReleaseSource for Fake {
        fn releases(&self) -> Result<Vec<Release>> {
            self.asked.set(self.asked.get() + 1);
            if self.fail.get() {
                anyhow::bail!("offline");
            }
            Ok(self.releases.clone())
        }
    }

    fn release(version: &str) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            notes_url: format!("https://example.test/{version}"),
            notes: String::new(),
            assets: Vec::new(),
        }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert!(v("4.10.0") > v("4.9.0"));
        assert!(v("v5.0.0") > v("4.99.99"));
        assert_eq!(Version::parse("v4.1.0"), Some(Version(4, 1, 0)));
        assert_eq!(Version::parse("4.1"), None);
        assert_eq!(Version::parse("4.1.0.1"), None);
        assert_eq!(Version::parse("4.1.0-rc1"), None, "pre-release");
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn github_answer_drops_drafts_prereleases_and_odd_tags() {
        let json = r#"[
            {"tag_name":"v4.2.0","html_url":"https://x/4.2.0","draft":false,"prerelease":false},
            {"tag_name":"v4.3.0-rc1","html_url":"https://x/rc","draft":false,"prerelease":true},
            {"tag_name":"v4.4.0","html_url":"https://x/draft","draft":true,"prerelease":false},
            {"tag_name":"nightly","html_url":"https://x/n","draft":false,"prerelease":false}
        ]"#;
        let releases = parse_releases(json).unwrap();
        assert_eq!(releases, vec![release_at("4.2.0", "https://x/4.2.0")]);
        assert!(parse_releases("not json").is_err());
    }

    fn release_at(version: &str, url: &str) -> Release {
        Release {
            version: v(version),
            notes_url: url.into(),
            notes: String::new(),
            assets: Vec::new(),
        }
    }

    #[test]
    fn a_release_brings_its_notes_and_none_is_empty() {
        let json = r###"[
            {"tag_name":"v4.2.0","html_url":"https://x/4.2.0","body":"## What's Changed\n* a fix"},
            {"tag_name":"v4.1.0","html_url":"https://x/4.1.0","body":null},
            {"tag_name":"v4.0.0","html_url":"https://x/4.0.0"}
        ]"###;
        let releases = parse_releases(json).unwrap();
        let notes: Vec<&str> = releases.iter().map(|r| r.notes.as_str()).collect();
        assert_eq!(notes, ["## What's Changed\n* a fix", "", ""]);
    }

    #[test]
    fn whats_new_is_each_newer_release_newest_first() {
        let releases = vec![
            release("4.1.0"),
            release("4.3.0"),
            release("4.0.0"),
            release("4.2.0"),
        ];
        let shown: Vec<String> = newer_than(v("4.1.0"), &releases)
            .iter()
            .map(|r| r.version.to_string())
            .collect();
        assert_eq!(shown, ["4.3.0", "4.2.0"]);
        assert!(newer_than(v("4.3.0"), &releases).is_empty());
    }

    #[test]
    fn a_newer_release_is_offered_with_how_far_behind_the_build_is() {
        let releases = vec![release("4.0.0"), release("4.1.0"), release("4.2.0")];
        let found = newest_above(v("4.0.0"), &releases).unwrap();
        assert_eq!(found.version, "4.2.0");
        assert_eq!(found.behind, 2);
        assert_eq!(found.notes_url, "https://example.test/4.2.0");
    }

    #[test]
    fn the_same_or_an_older_release_is_nothing() {
        let releases = vec![release("4.0.0"), release("3.9.0")];
        assert_eq!(newest_above(v("4.0.0"), &releases), None);
        assert_eq!(
            newest_above(v("4.0.1"), &releases),
            None,
            "ahead of released"
        );
        assert_eq!(newest_above(v("4.0.0"), &[]), None);
    }

    #[test]
    fn check_reports_a_newer_release_and_remembers_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.0.0", "4.1.0"]);

        let found = check(&fake, v("4.0.0"), 1_000, &file).unwrap();
        assert_eq!(found.version, "4.1.0");

        // Within the interval, the answer comes from memory: GitHub isn't asked.
        assert!(check(&fake, v("4.0.0"), 1_000 + CHECK_EVERY - 1, &file).is_some());
        assert_eq!(fake.asked.get(), 1);

        // Once it's up, it is.
        check(&fake, v("4.0.0"), 1_000 + CHECK_EVERY, &file);
        assert_eq!(fake.asked.get(), 2);
    }

    #[test]
    fn once_updated_the_remembered_release_is_no_longer_news() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        assert!(check(&fake, v("4.0.0"), 0, &file).is_some());
        assert_eq!(check(&fake, v("4.1.0"), 60, &file), None);
    }

    #[test]
    fn later_hides_a_release_for_a_day_then_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        let found = check(&fake, v("4.0.0"), 0, &file).unwrap();

        snooze(&file, &found.version, 100).unwrap();
        assert_eq!(check(&fake, v("4.0.0"), 200, &file), None, "snoozed");
        assert_eq!(
            check(&fake, v("4.0.0"), 100 + SNOOZE_FOR - 1, &file),
            None,
            "just before the day is up"
        );
        assert!(
            check(&fake, v("4.0.0"), 100 + SNOOZE_FOR, &file).is_some(),
            "a day later it is back"
        );
    }

    #[test]
    fn a_still_newer_release_shows_at_once_despite_the_snooze() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let mut fake = Fake::with(&["4.1.0"]);
        let found = check(&fake, v("4.0.0"), 0, &file).unwrap();
        snooze(&file, &found.version, 10).unwrap();

        fake.releases.push(release("4.2.0"));
        let found = check(&fake, v("4.0.0"), CHECK_EVERY, &file).unwrap();
        assert_eq!(found.version, "4.2.0");
    }

    #[test]
    fn a_snooze_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        check(&fake, v("4.0.0"), 0, &file);
        snooze(&file, "4.1.0", 5).unwrap();

        // A new process has only the file.
        let again = Fake::with(&["4.1.0"]);
        assert_eq!(check(&again, v("4.0.0"), 50, &file), None);
        assert_eq!(again.asked.get(), 0, "not asked: the interval isn't up");
    }

    #[test]
    fn a_failed_check_is_silent_and_retried_within_the_hour() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        fake.fail.set(true);

        assert_eq!(check(&fake, v("4.0.0"), 0, &file), None);
        assert_eq!(fake.asked.get(), 1);
        // Not on every call...
        check(&fake, v("4.0.0"), 60, &file);
        assert_eq!(fake.asked.get(), 1);
        // ...but not a whole day either.
        fake.fail.set(false);
        assert!(check(&fake, v("4.0.0"), RETRY_AFTER_FAILURE, &file).is_some());
    }

    #[test]
    fn a_failure_keeps_what_was_already_known() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        check(&fake, v("4.0.0"), 0, &file);

        fake.fail.set(true);
        assert!(check(&fake, v("4.0.0"), CHECK_EVERY, &file).is_some());
    }

    #[test]
    fn a_clock_set_back_does_not_hold_the_check_off() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        check(&fake, v("4.0.0"), 10 * CHECK_EVERY, &file);
        check(&fake, v("4.0.0"), 0, &file);
        assert_eq!(fake.asked.get(), 2);
    }

    #[test]
    fn a_next_check_from_the_old_daily_interval_is_asked_at_once() {
        // A build before ticket 285 wrote `next_check` a day ahead; the new
        // build mustn't wait that day out.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let now = 1_000_000;
        fs::write(
            &file,
            format!(
                r#"{{"next_check":{},"releases":[]}}"#,
                now + 24 * 60 * 60 - 60
            ),
        )
        .unwrap();
        let fake = Fake::with(&["4.1.0"]);
        assert!(check(&fake, v("4.0.0"), now, &file).is_some());
        assert_eq!(fake.asked.get(), 1);
    }

    #[test]
    fn a_corrupt_state_file_is_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        fs::write(&file, "{ not json").unwrap();
        let fake = Fake::with(&["4.1.0"]);
        assert!(check(&fake, v("4.0.0"), 0, &file).is_some());
    }

    #[test]
    fn an_unwritable_state_dir_still_gives_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the directory should be: every save fails.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let fake = Fake::with(&["4.1.0"]);
        let found = check(&fake, v("4.0.0"), 0, &blocker.join("update.json"));
        assert_eq!(found.unwrap().version, "4.1.0");
    }

    #[test]
    fn the_state_lives_outside_any_repository() {
        let state = tempfile::tempdir().unwrap();
        // Safety: this test alone sets XDG_STATE_HOME, before reading it.
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        assert_eq!(
            state_path().unwrap(),
            state.path().join("kanrite/update.json")
        );
    }

    #[test]
    fn the_check_can_be_switched_off_and_defaults_on() {
        let mut config = Config::default();
        assert!(enabled(&config));
        for off in ["off", "0", "no", "false", "OFF"] {
            config.set("update-check", off);
            assert!(!enabled(&config), "{off}");
        }
        config.set("update-check", "on");
        assert!(enabled(&config));
    }

    #[test]
    fn a_check_by_hand_asks_now_whatever_the_interval_says() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let mut fake = Fake::with(&["4.1.0"]);
        // An automatic check an hour ago: not due again for two more.
        check(&fake, v("4.0.0"), 0, &file);
        fake.releases.push(release("4.2.0"));
        let found = check_now(&fake, v("4.0.0"), 3_600, &file);
        assert_eq!(fake.asked.get(), 2);
        match found {
            Manual::Newer(found) => assert_eq!(found.version, "4.2.0"),
            other => panic!("{other:?}"),
        }
        assert_eq!(last_checked(&file), Some(3_600));
        // What it found is what the automatic check now answers with.
        assert_eq!(
            check(&fake, v("4.0.0"), 3_700, &file).unwrap().version,
            "4.2.0"
        );
        assert_eq!(fake.asked.get(), 2);
    }

    #[test]
    fn checks_by_hand_reach_github_at_most_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.0.0"]);
        assert_eq!(check_now(&fake, v("4.0.0"), 1_000, &file), Manual::Current);
        assert_eq!(check_now(&fake, v("4.0.0"), 1_030, &file), Manual::Current);
        assert_eq!(fake.asked.get(), 1, "the second click answers from memory");
        check_now(&fake, v("4.0.0"), 1_000 + MANUAL_EVERY, &file);
        assert_eq!(fake.asked.get(), 2);
    }

    #[test]
    fn a_check_by_hand_says_when_github_cant_be_reached() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        fake.fail.set(true);
        assert!(matches!(
            check_now(&fake, v("4.0.0"), 0, &file),
            Manual::Failed(why) if why.contains("offline")
        ));
        assert_eq!(last_checked(&file), None);
        // Nothing found, so the next click asks again at once.
        fake.fail.set(false);
        assert!(matches!(
            check_now(&fake, v("4.0.0"), 5, &file),
            Manual::Newer(_)
        ));
    }

    #[test]
    fn a_check_by_hand_clears_later() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let fake = Fake::with(&["4.1.0"]);
        check(&fake, v("4.0.0"), 0, &file);
        snooze(&file, "4.1.0", 10).unwrap();
        assert_eq!(check(&fake, v("4.0.0"), 20, &file), None, "snoozed");
        assert!(matches!(
            check_now(&fake, v("4.0.0"), 30, &file),
            Manual::Newer(_)
        ));
        assert!(
            check(&fake, v("4.0.0"), 40, &file).is_some(),
            "Later is gone"
        );
    }
}
