//! One poll cycle (fetch → dedupe → filter → play) and the daemon loop around it.
//! Behavior: `docs/architecture.md` ("State and deduplication", "Filters", "Daemon loop").

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use chrono_tz::Tz;
use tracing::{debug, error, info, warn};

use crate::clock::Clock;
use crate::config::Config;
use crate::hours::QuietHours;
use crate::http::HttpError;
use crate::player::Player;
use crate::retry::Backoff;
use crate::source::{MergeEvent, MergeSource};
use crate::state::{PlayRecord, SourceState, State, StateStore};
use crate::tags;

/// How far before the last successful poll the next one looks: covers search-index lag and
/// clock skew. The `seen` set removes the duplicates it causes.
pub const OVERLAP: Duration = Duration::from_secs(10 * 60);
const PLAY_GAP: Duration = Duration::from_secs(1);
const PLAY_RETRY_DELAY: Duration = Duration::from_secs(5);
/// A source whose token was rejected is retried this often.
const AUTH_RETRY: Duration = Duration::from_secs(10 * 60);
const POLL_BACKOFF: Backoff = Backoff {
    initial: Duration::from_secs(60),
    max: Duration::from_secs(10 * 60),
};
/// Longest single sleep, so a shutdown or a resume from suspend is noticed quickly.
const SLEEP_CHUNK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct Settings {
    pub poll_interval: Duration,
    pub catch_up: Duration,
    pub max_plays_per_poll: usize,
    pub quiet_hours: Option<QuietHours>,
    pub timezone: Tz,
    pub tags_dir: PathBuf,
    /// Fetch and log, but play nothing and never write state.
    pub dry_run: bool,
}

impl Settings {
    pub fn from_config(config: &Config, dry_run: bool) -> Self {
        Self {
            poll_interval: config.poll_interval,
            catch_up: config.catch_up,
            max_plays_per_poll: config.max_plays_per_poll,
            quiet_hours: config.quiet_hours,
            timezone: config.timezone,
            tags_dir: config.tags_dir.clone(),
            dry_run,
        }
    }

    /// Merges older than this (relative to now) don't play: at least `CATCH_UP_MINUTES`, and
    /// always the normal poll window (interval + overlap), so `CATCH_UP_MINUTES=0` still plays
    /// merges found by regular polls.
    fn max_age(&self) -> Duration {
        self.catch_up.max(self.poll_interval + OVERLAP)
    }

    /// `seen` entries older than this are pruned; they would fail the catch-up filter anyway.
    fn seen_retention(&self) -> Duration {
        self.max_age() + OVERLAP + Duration::from_secs(86_400)
    }
}

/// Why a new merge did not play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Older than the catch-up window (machine was asleep, off or offline).
    CatchUp,
    QuietHours,
    /// Over `MAX_PLAYS_PER_POLL` in this cycle.
    Burst,
    /// Neither the author nor the default has a tag file.
    NoTag,
    /// Shutting down.
    Shutdown,
    /// The state couldn't be saved; playing could replay it after a restart.
    StateNotSaved,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CatchUp => "catch-up: merged too long ago",
            Self::QuietHours => "quiet hours",
            Self::Burst => "burst: MAX_PLAYS_PER_POLL reached",
            Self::NoTag => "no tag file",
            Self::Shutdown => "shutting down",
            Self::StateNotSaved => "state not saved",
        })
    }
}

/// What one cycle did; used by tests, `once` and `simulate`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CycleReport {
    /// Sources polled successfully.
    pub polled: usize,
    /// Sources whose poll failed.
    pub failed: usize,
    /// Sources seen for the first time (results marked seen, nothing played).
    pub first_starts: usize,
    /// New (never seen) merges.
    pub new: usize,
    /// Event keys played (or that would have played in a dry run).
    pub played: Vec<String>,
    /// Plays that failed even after the retry.
    pub play_failures: usize,
    pub skipped: Vec<(String, SkipReason)>,
}

struct Slot {
    source: Box<dyn MergeSource>,
    failures: u32,
    /// `None`: due now.
    next_poll: Option<DateTime<Utc>>,
}

pub struct Poller<'a> {
    slots: Vec<Slot>,
    player: &'a dyn Player,
    store: &'a dyn StateStore,
    clock: &'a dyn Clock,
    settings: Settings,
    state: State,
}

impl<'a> Poller<'a> {
    pub fn new(
        sources: Vec<Box<dyn MergeSource>>,
        player: &'a dyn Player,
        store: &'a dyn StateStore,
        clock: &'a dyn Clock,
        settings: Settings,
    ) -> Self {
        Self {
            slots: sources
                .into_iter()
                .map(|source| Slot {
                    source,
                    failures: 0,
                    next_poll: None,
                })
                .collect(),
            player,
            store,
            clock,
            settings,
            state: store.load(),
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Polls forever (every source on its own schedule) until a shutdown is requested.
    pub fn run(&mut self) {
        while !self.clock.shutdown_requested() {
            self.cycle();
            if !self.wait_until(self.next_due()) {
                break;
            }
        }
        info!("shutting down");
    }

    /// Polls every source that is due, then plays what passed the filters.
    pub fn cycle(&mut self) -> CycleReport {
        let now = self.clock.now();
        let mut report = CycleReport::default();
        let mut fresh: Vec<MergeEvent> = Vec::new();

        for i in 0..self.slots.len() {
            if self.slots[i].next_poll.is_some_and(|due| due > now) {
                continue;
            }
            self.poll_source(i, now, &mut report, &mut fresh);
        }
        report.new = fresh.len();

        let saved = self.save(now);
        self.play_all(fresh, now, saved, &mut report);
        if !report.played.is_empty() {
            self.save(now);
        }
        report
    }

    fn poll_source(
        &mut self,
        i: usize,
        now: DateTime<Utc>,
        report: &mut CycleReport,
        fresh: &mut Vec<MergeEvent>,
    ) {
        let poll_interval = self.settings.poll_interval;
        let slot = &mut self.slots[i];
        let id = slot.source.id().to_owned();
        let previous = self.state.sources.get(&id).map(|s| s.last_success);
        let since = poll_since(previous.unwrap_or(now));

        match slot.source.poll(since) {
            Ok(events) => {
                report.polled += 1;
                if slot.failures > 0 {
                    info!(source = id, "polling works again");
                }
                slot.failures = 0;
                slot.next_poll = Some(now + to_delta(poll_interval));

                if previous.is_none() {
                    // First start: everything that already exists is history, not news.
                    report.first_starts += 1;
                    for event in &events {
                        self.state.mark_seen(&event.key, event.merged_at);
                    }
                    info!(
                        source = id,
                        existing = events.len(),
                        "first start: existing merges marked as seen, nothing plays"
                    );
                } else {
                    let before = fresh.len();
                    for event in events {
                        if self.state.mark_seen(&event.key, event.merged_at) {
                            info!(source = id, merge = %event, title = event.title, url = event.url, "new merge");
                            fresh.push(event);
                        }
                    }
                    debug!(source = id, new = fresh.len() - before, "poll done");
                }
                let login = slot.source.cached_login().map(str::to_owned);
                self.state.sources.insert(
                    id,
                    SourceState {
                        login,
                        last_success: now,
                    },
                );
            }
            Err(e) => {
                report.failed += 1;
                slot.failures += 1;
                let wait = match &e {
                    HttpError::RateLimited { wait } => (*wait).max(poll_interval),
                    HttpError::Unauthorized { .. } => AUTH_RETRY,
                    _ => POLL_BACKOFF.delay(slot.failures - 1).max(poll_interval),
                };
                slot.next_poll = Some(now + to_delta(wait));
                if matches!(e, HttpError::Unauthorized { .. }) {
                    error!(source = id, error = %e, retry_in_secs = wait.as_secs(), "source paused");
                } else {
                    warn!(source = id, error = %e, failures = slot.failures, retry_in_secs = wait.as_secs(), "poll failed");
                }
            }
        }
    }

    fn play_all(
        &mut self,
        mut fresh: Vec<MergeEvent>,
        now: DateTime<Utc>,
        saved: bool,
        report: &mut CycleReport,
    ) {
        fresh.sort_by_key(|e| e.merged_at);
        let oldest_playable = now - to_delta(self.settings.max_age());
        let quiet = self
            .settings
            .quiet_hours
            .is_some_and(|q| q.contains(now, self.settings.timezone));
        let mut attempts = 0;

        for event in fresh {
            let skip = if event.merged_at < oldest_playable {
                Some(SkipReason::CatchUp)
            } else if quiet {
                Some(SkipReason::QuietHours)
            } else if attempts >= self.settings.max_plays_per_poll {
                Some(SkipReason::Burst)
            } else if !saved {
                Some(SkipReason::StateNotSaved)
            } else if self.clock.shutdown_requested() {
                Some(SkipReason::Shutdown)
            } else {
                None
            };
            if let Some(reason) = skip {
                info!(merge = %event, %reason, "skipped");
                report.skipped.push((event.key, reason));
                continue;
            }

            let Some(tag) =
                tags::lookup(&self.settings.tags_dir, Some(event.platform), &event.author)
            else {
                warn!(merge = %event, tags_dir = %self.settings.tags_dir.display(), "no tag file (not even default.*); skipped");
                report.skipped.push((event.key, SkipReason::NoTag));
                continue;
            };
            let tag_name = tags::display_name(&self.settings.tags_dir, &tag);

            if attempts > 0 && !self.clock.sleep(PLAY_GAP) {
                report.skipped.push((event.key, SkipReason::Shutdown));
                continue;
            }
            attempts += 1;
            if self.settings.dry_run {
                info!(merge = %event, tag = tag_name, "dry run: would play");
                report.played.push(event.key);
                continue;
            }

            info!(merge = %event, tag = tag_name, "playing producer tag");
            if self.play_with_retry(&tag) {
                self.state.record_play(PlayRecord {
                    key: event.key.clone(),
                    repo: event.repo.clone(),
                    number: event.number,
                    author: event.author.clone(),
                    tag: tag_name,
                    played_at: self.clock.now(),
                });
                report.played.push(event.key);
            } else {
                report.play_failures += 1;
            }
        }
    }

    /// One retry after a short pause; a tag that arrives minutes late is worse than none.
    fn play_with_retry(&self, tag: &std::path::Path) -> bool {
        match self.player.play(tag) {
            Ok(()) => return true,
            Err(e) => warn!(error = %e, "playing failed; retrying in 5s"),
        }
        if !self.clock.sleep(PLAY_RETRY_DELAY) {
            return false;
        }
        match self.player.play(tag) {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "playing failed again; dropped");
                false
            }
        }
    }

    /// Prunes and writes the state. Returns whether it is safely on disk (always `true` in a dry
    /// run, which never writes).
    fn save(&mut self, now: DateTime<Utc>) -> bool {
        if self.settings.dry_run {
            return true;
        }
        self.state.prune(now, self.settings.seen_retention());
        match self.store.save(&self.state) {
            Ok(()) => true,
            Err(e) => {
                error!(error = format!("{e:#}"), "cannot save state");
                false
            }
        }
    }

    fn next_due(&self) -> DateTime<Utc> {
        let now = self.clock.now();
        self.slots
            .iter()
            .map(|s| s.next_poll.unwrap_or(now))
            .min()
            .unwrap_or(now + to_delta(self.settings.poll_interval))
    }

    /// Sleeps in short chunks until `deadline` on the wall clock. Returns `false` on shutdown.
    /// The wall clock keeps running while the machine is suspended, so after a resume the
    /// deadline is simply already past and the next poll happens right away.
    fn wait_until(&self, deadline: DateTime<Utc>) -> bool {
        loop {
            if self.clock.shutdown_requested() {
                return false;
            }
            let now = self.clock.now();
            if now >= deadline {
                return true;
            }
            let step = (deadline - now)
                .to_std()
                .unwrap_or_default()
                .min(SLEEP_CHUNK);
            if !self.clock.sleep(step) {
                return false;
            }
        }
    }
}

/// `since` for a poll: the last success minus [`OVERLAP`], rounded down to the hour. The
/// rounding keeps the request URL identical between polls, so `ETag`/`If-None-Match` can answer
/// `304 Not Modified` (free on GitHub's rate limit) when nothing changed.
pub fn poll_since(last_success: DateTime<Utc>) -> DateTime<Utc> {
    let since = last_success - to_delta(OVERLAP);
    since.duration_trunc(TimeDelta::hours(1)).unwrap_or(since)
}

fn to_delta(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::clock::testing::FakeClock;
    use crate::player::testing::RecordingPlayer;
    use crate::source::Platform;
    use crate::source::fake::{ScriptedSource, event};
    use crate::state::MemoryStore;

    const NOW: &str = "2026-10-04T14:00:00Z";

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    fn mins_ago(clock: &FakeClock, minutes: i64) -> DateTime<Utc> {
        clock.now() - TimeDelta::minutes(minutes)
    }

    struct Fixture {
        tags: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let tags = tempfile::tempdir().unwrap();
            fs::write(tags.path().join("default.wav"), b"RIFF").unwrap();
            Self { tags }
        }

        fn settings(&self) -> Settings {
            Settings {
                poll_interval: Duration::from_secs(60),
                catch_up: Duration::from_secs(30 * 60),
                max_plays_per_poll: 3,
                quiet_hours: None,
                timezone: Tz::UTC,
                tags_dir: self.tags.path().to_owned(),
                dry_run: false,
            }
        }

        fn tag(&self, rel: &str) -> PathBuf {
            let path = self.tags.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"RIFF").unwrap();
            path
        }

        fn default_tag(&self) -> PathBuf {
            self.tags.path().join("default.wav")
        }
    }

    /// State where `github` was last polled successfully a minute ago (not a first start).
    fn known_state(clock: &FakeClock) -> State {
        let mut state = State::default();
        state.sources.insert(
            "github".into(),
            SourceState {
                login: Some("you".into()),
                last_success: mins_ago(clock, 1),
            },
        );
        state
    }

    fn gh(key: &str, author: &str, merged_at: DateTime<Utc>) -> MergeEvent {
        event(key, Platform::GitHub, author, merged_at)
    }

    fn boxed(source: ScriptedSource) -> Vec<Box<dyn MergeSource>> {
        vec![Box::new(source)]
    }

    #[test]
    fn first_start_marks_everything_seen_and_plays_nothing() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::default();
        let source = ScriptedSource::new("github", Platform::GitHub)
            .then(Ok(vec![
                gh("github:1", "you", mins_ago(&clock, 2)),
                gh("github:2", "you", mins_ago(&clock, 50)),
            ]))
            .then(Ok(vec![gh("github:1", "you", mins_ago(&clock, 2))]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());

        let report = poller.cycle();
        assert_eq!(report.first_starts, 1);
        assert_eq!(report.new, 0);
        assert!(player.played().is_empty());
        let state = store.state();
        assert!(state.seen.contains_key("github:1") && state.seen.contains_key("github:2"));
        assert_eq!(state.sources["github"].last_success, at(NOW));
        assert_eq!(state.sources["github"].login.as_deref(), Some("you"));

        clock.advance(TimeDelta::minutes(1));
        let report = poller.cycle();
        assert_eq!(report.new, 0, "already seen on first start");
        assert!(player.played().is_empty());
    }

    #[test]
    fn same_event_in_two_polls_plays_once_and_survives_restart() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let e = gh("github:7", "you", mins_ago(&clock, 1));
        let source = ScriptedSource::new("github", Platform::GitHub)
            .then(Ok(vec![e.clone()]))
            .then(Ok(vec![e.clone()]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());

        assert_eq!(poller.cycle().played, ["github:7"]);
        clock.advance(TimeDelta::minutes(1));
        assert!(poller.cycle().played.is_empty());
        assert_eq!(player.played(), [fx.default_tag()]);

        // A restart (new poller, same store) with the event still in the overlap window.
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![e]));
        let mut restarted = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        assert_eq!(restarted.cycle().new, 0);
        assert_eq!(player.played().len(), 1);
        assert_eq!(store.state().last_played[0].key, "github:7");
    }

    #[test]
    fn seen_is_persisted_before_playing() {
        struct CheckingPlayer<'s> {
            store: &'s MemoryStore,
            checked: RefCell<bool>,
        }
        impl Player for CheckingPlayer<'_> {
            fn play(&self, _: &Path) -> Result<(), crate::exec::ExecError> {
                assert!(self.store.state().seen.contains_key("github:1"));
                *self.checked.borrow_mut() = true;
                Ok(())
            }
            fn program(&self) -> Option<&str> {
                None
            }
        }

        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let store = MemoryStore::new(known_state(&clock));
        let player = CheckingPlayer {
            store: &store,
            checked: RefCell::new(false),
        };
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        poller.cycle();
        assert!(*player.checked.borrow());
    }

    #[test]
    fn nothing_plays_when_state_cannot_be_saved() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::failing(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        let report = poller.cycle();
        assert!(player.played().is_empty());
        assert_eq!(
            report.skipped,
            [("github:1".into(), SkipReason::StateNotSaved)]
        );
    }

    #[test]
    fn catch_up_after_sleep() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let mut state = known_state(&clock);
        // Laptop closed 3 hours ago.
        state.sources.get_mut("github").unwrap().last_success = mins_ago(&clock, 180);
        let store = MemoryStore::new(state);
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![
            gh("github:old", "you", mins_ago(&clock, 120)),
            gh("github:recent", "you", mins_ago(&clock, 20)),
        ]));
        let polls = source.polls();
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());

        let report = poller.cycle();
        assert_eq!(report.played, ["github:recent"]);
        assert_eq!(report.skipped, [("github:old".into(), SkipReason::CatchUp)]);
        assert!(store.state().seen.contains_key("github:old"));
        // since = last success - overlap, rounded down to the hour.
        assert_eq!(polls.borrow()[0], at("2026-10-04T10:00:00Z"));
    }

    #[test]
    fn catch_up_zero_still_plays_the_regular_poll_window() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![
            gh("github:lagged", "you", mins_ago(&clock, 5)),
            gh("github:old", "you", mins_ago(&clock, 25)),
        ]));
        let settings = Settings {
            catch_up: Duration::ZERO,
            ..fx.settings()
        };
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, settings);
        let report = poller.cycle();
        assert_eq!(report.played, ["github:lagged"]);
        assert_eq!(report.skipped, [("github:old".into(), SkipReason::CatchUp)]);
    }

    #[test]
    fn quiet_hours_record_but_do_not_play() {
        let fx = Fixture::new();
        let clock = FakeClock::at("2026-10-04T23:30:00Z");
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let settings = Settings {
            quiet_hours: Some("22:00-08:00".parse().unwrap()),
            ..fx.settings()
        };
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, settings);
        let report = poller.cycle();
        assert!(player.played().is_empty());
        assert_eq!(
            report.skipped,
            [("github:1".into(), SkipReason::QuietHours)]
        );
        assert!(store.state().seen.contains_key("github:1"));
    }

    #[test]
    fn burst_cap_plays_oldest_first_with_gaps() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let events = (1..=5)
            .map(|n| gh(&format!("github:{n}"), "you", mins_ago(&clock, 10 - n)))
            .rev()
            .collect();
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(events));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());

        let report = poller.cycle();
        assert_eq!(report.played, ["github:1", "github:2", "github:3"]);
        assert_eq!(
            report.skipped,
            [
                ("github:4".into(), SkipReason::Burst),
                ("github:5".into(), SkipReason::Burst)
            ]
        );
        assert_eq!(clock.sleeps(), [PLAY_GAP, PLAY_GAP]);
    }

    #[test]
    fn team_tags_are_looked_up_per_author() {
        let fx = Fixture::new();
        let alice = fx.tag("github/alice.ogg");
        let bob = fx.tag("bob.mp3");
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![
            gh("github:1", "Alice", mins_ago(&clock, 3)),
            gh("github:2", "bob", mins_ago(&clock, 2)),
            gh("github:3", "../../etc/passwd", mins_ago(&clock, 1)),
        ]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        poller.cycle();
        assert_eq!(player.played(), [alice, bob, fx.default_tag()]);
        assert_eq!(store.state().last_played[0].tag, "default.wav");
        assert_eq!(store.state().last_played[2].tag, "github/alice.ogg");
    }

    #[test]
    fn missing_tag_skips() {
        let fx = Fixture::new();
        fs::remove_file(fx.default_tag()).unwrap();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        let report = poller.cycle();
        assert_eq!(report.skipped, [("github:1".into(), SkipReason::NoTag)]);
    }

    #[test]
    fn failed_play_is_retried_once_then_dropped() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let store = MemoryStore::new(known_state(&clock));

        let player = RecordingPlayer::failing(1);
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        let report = poller.cycle();
        assert_eq!(report.played, ["github:1"]);
        assert_eq!(player.played().len(), 2);
        assert_eq!(clock.sleeps(), [PLAY_RETRY_DELAY]);

        let player = RecordingPlayer::failing(5);
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:2",
            "you",
            mins_ago(&clock, 1),
        )]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        let report = poller.cycle();
        assert!(report.played.is_empty());
        assert_eq!(report.play_failures, 1);
        assert_eq!(player.played().len(), 2);
        assert!(
            store.state().seen.contains_key("github:2"),
            "never retried later"
        );
    }

    #[test]
    fn dry_run_plays_nothing_and_writes_nothing() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![gh(
            "github:1",
            "you",
            mins_ago(&clock, 1),
        )]));
        let settings = Settings {
            dry_run: true,
            ..fx.settings()
        };
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, settings);
        let report = poller.cycle();
        assert_eq!(report.played, ["github:1"]);
        assert!(player.played().is_empty());
        assert_eq!(store.saves(), 0);
    }

    #[test]
    fn failing_source_backs_off_without_blocking_the_other() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let mut state = known_state(&clock);
        state.sources.insert(
            "gitlab:gitlab.com".into(),
            SourceState {
                login: None,
                last_success: mins_ago(&clock, 1),
            },
        );
        let store = MemoryStore::new(state);
        let failing = ScriptedSource::new("github", Platform::GitHub)
            .then(Err(HttpError::Network("down".into())))
            .then(Err(HttpError::Network("down".into())));
        let failing_polls = failing.polls();
        let healthy =
            ScriptedSource::new("gitlab:gitlab.com", Platform::GitLab).then(Ok(vec![event(
                "gitlab:gitlab.com:1",
                Platform::GitLab,
                "you",
                mins_ago(&clock, 1),
            )]));
        let sources: Vec<Box<dyn MergeSource>> = vec![Box::new(failing), Box::new(healthy)];
        let mut poller = Poller::new(sources, &player, &store, &clock, fx.settings());

        let report = poller.cycle();
        assert_eq!((report.polled, report.failed), (1, 1));
        assert_eq!(report.played, ["gitlab:gitlab.com:1"]);
        // The failed source keeps its old cursor, so nothing is lost inside the catch-up window.
        assert_eq!(
            store.state().sources["github"].last_success,
            mins_ago(&clock, 1)
        );

        // Backoff: 1 min, then 2 min.
        clock.advance(TimeDelta::seconds(60));
        poller.cycle();
        assert_eq!(failing_polls.borrow().len(), 2);
        clock.advance(TimeDelta::seconds(60));
        poller.cycle();
        assert_eq!(failing_polls.borrow().len(), 2, "still backing off");
        clock.advance(TimeDelta::seconds(60));
        poller.cycle();
        assert_eq!(failing_polls.borrow().len(), 3);
    }

    #[test]
    fn rate_limit_and_auth_errors_set_the_next_poll() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub)
            .then(Err(HttpError::RateLimited {
                wait: Duration::from_secs(300),
            }))
            .then(Err(HttpError::Unauthorized { status: 401 }));
        let polls = source.polls();
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());

        poller.cycle();
        assert_eq!(poller.next_due(), clock.now() + TimeDelta::seconds(300));
        clock.advance(TimeDelta::seconds(300));
        poller.cycle();
        assert_eq!(polls.borrow().len(), 2);
        assert_eq!(poller.next_due(), clock.now() + TimeDelta::minutes(10));
    }

    #[test]
    fn run_loop_polls_on_interval_and_stops_on_shutdown() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW).interrupt_after(150);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub);
        let polls = source.polls();
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        poller.run();
        // 150 one-second sleeps = 2.5 intervals: polls at t=0, 60, 120.
        assert_eq!(polls.borrow().len(), 3);
        assert!(clock.sleeps().iter().all(|s| *s <= SLEEP_CHUNK));
    }

    #[test]
    fn shutdown_during_burst_stops_further_plays() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW).interrupt_after(0);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let source = ScriptedSource::new("github", Platform::GitHub).then(Ok(vec![
            gh("github:1", "you", mins_ago(&clock, 2)),
            gh("github:2", "you", mins_ago(&clock, 1)),
        ]));
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        let report = poller.cycle();
        assert_eq!(report.played, ["github:1"], "the running play finishes");
        assert_eq!(report.skipped, [("github:2".into(), SkipReason::Shutdown)]);
        assert!(store.state().seen.contains_key("github:2"));
    }

    #[test]
    fn old_seen_entries_are_pruned() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let mut state = known_state(&clock);
        state.mark_seen("github:ancient", clock.now() - TimeDelta::days(3));
        state.mark_seen("github:recent", mins_ago(&clock, 30));
        let store = MemoryStore::new(state);
        let source = ScriptedSource::new("github", Platform::GitHub);
        let mut poller = Poller::new(boxed(source), &player, &store, &clock, fx.settings());
        poller.cycle();
        let seen = store.state().seen;
        assert!(!seen.contains_key("github:ancient"));
        assert!(seen.contains_key("github:recent"));
    }

    #[test]
    fn poll_since_rounds_down_to_the_hour() {
        assert_eq!(
            poll_since(at("2026-10-04T14:05:00Z")),
            at("2026-10-04T13:00:00Z")
        );
        assert_eq!(
            poll_since(at("2026-10-04T14:15:00Z")),
            at("2026-10-04T14:00:00Z")
        );
    }

    #[test]
    fn wait_until_returns_at_once_after_a_resume() {
        let fx = Fixture::new();
        let clock = FakeClock::at(NOW);
        let player = RecordingPlayer::default();
        let store = MemoryStore::new(known_state(&clock));
        let poller = Poller::new(
            boxed(ScriptedSource::new("github", Platform::GitHub)),
            &player,
            &store,
            &clock,
            fx.settings(),
        );
        let deadline = clock.now() + TimeDelta::minutes(1);
        clock.advance(TimeDelta::hours(2));
        assert!(poller.wait_until(deadline));
        assert!(clock.sleeps().is_empty());
    }
}
