//! Persistent state: per-source cursors, the `seen` set and the last plays.
//!
//! The `seen` set is what guarantees each merge plays at most once, across restarts.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub const STATE_VERSION: u32 = 1;
/// How many plays `status` can show.
pub const LAST_PLAYED_MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceState>,
    /// Event key → merge time.
    #[serde(default)]
    pub seen: BTreeMap<String, DateTime<Utc>>,
    /// Most recent first.
    #[serde(default)]
    pub last_played: Vec<PlayRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    pub last_success: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayRecord {
    pub key: String,
    pub repo: String,
    pub number: u64,
    pub author: String,
    /// Tag file name relative to `TAGS_DIR`.
    pub tag: String,
    pub played_at: DateTime<Utc>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            sources: BTreeMap::new(),
            seen: BTreeMap::new(),
            last_played: Vec::new(),
        }
    }
}

impl State {
    /// Inserts `key`; returns `true` if it was not seen before.
    pub fn mark_seen(&mut self, key: &str, merged_at: DateTime<Utc>) -> bool {
        self.seen.insert(key.to_owned(), merged_at).is_none()
    }

    /// Forgets merges older than `keep`. They can never play again anyway: the catch-up filter
    /// drops them long before they get here.
    pub fn prune(&mut self, now: DateTime<Utc>, keep: Duration) {
        let cutoff = now - TimeDelta::from_std(keep).unwrap_or(TimeDelta::MAX);
        self.seen.retain(|_, merged_at| *merged_at >= cutoff);
    }

    pub fn record_play(&mut self, record: PlayRecord) {
        self.last_played.insert(0, record);
        self.last_played.truncate(LAST_PLAYED_MAX);
    }
}

pub trait StateStore {
    /// Never fails: a missing or unreadable state is a first start (nothing replays).
    fn load(&self) -> State;
    fn save(&self, state: &State) -> Result<()>;
}

/// `state.json` in `DATA_DIR`, written atomically (temp file, fsync, rename).
pub struct JsonFileStore {
    path: PathBuf,
}

impl JsonFileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl StateStore for JsonFileStore {
    fn load(&self) -> State {
        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return State::default(),
            Err(e) => {
                warn!(path = %self.path.display(), error = %e, "cannot read state; starting fresh");
                return State::default();
            }
        };
        match serde_json::from_str::<State>(&content) {
            Ok(state) if state.version == STATE_VERSION => state,
            Ok(state) => {
                warn!(
                    version = state.version,
                    "unknown state version; starting fresh"
                );
                State::default()
            }
            Err(e) => {
                warn!(path = %self.path.display(), error = %e, "corrupt state file; starting fresh (nothing replays)");
                State::default()
            }
        }
    }

    fn save(&self, state: &State) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(state).context("serializing state")?;
        let write = || -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&json)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            // Persist the rename itself. Best effort: not every platform can open a directory.
            if let Ok(dir) = File::open(dir) {
                let _ = dir.sync_all();
            }
            Ok(())
        };
        write().with_context(|| format!("writing {}", self.path.display()))
    }
}

/// In-memory store, for `simulate` and tests.
#[derive(Default)]
pub struct MemoryStore {
    state: RefCell<State>,
    saves: RefCell<usize>,
    fail: bool,
}

impl MemoryStore {
    pub fn new(state: State) -> Self {
        Self {
            state: RefCell::new(state),
            ..Self::default()
        }
    }

    /// A store whose saves fail, like a full disk.
    pub fn failing(state: State) -> Self {
        Self {
            fail: true,
            ..Self::new(state)
        }
    }

    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }

    pub fn saves(&self) -> usize {
        *self.saves.borrow()
    }
}

impl StateStore for MemoryStore {
    fn load(&self) -> State {
        self.state.borrow().clone()
    }

    fn save(&self, state: &State) -> Result<()> {
        if self.fail {
            anyhow::bail!("disk full (simulated)");
        }
        *self.state.borrow_mut() = state.clone();
        *self.saves.borrow_mut() += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    fn sample() -> State {
        let mut state = State::default();
        state.sources.insert(
            "github".into(),
            SourceState {
                login: Some("VillegasMich".into()),
                last_success: at("2026-10-04T14:02:11Z"),
            },
        );
        state.mark_seen("github:1", at("2026-10-04T13:58:40Z"));
        state
    }

    #[test]
    fn round_trips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFileStore::new(dir.path().join("nested/state.json"));
        assert_eq!(store.load(), State::default(), "missing file = first start");

        let state = sample();
        store.save(&state).unwrap();
        assert_eq!(store.load(), state);
        assert!(!dir.path().join("nested/state.json.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_a_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(JsonFileStore::new(path.clone()).load(), State::default());

        fs::write(&path, r#"{"version": 99, "sources": {}}"#).unwrap();
        assert_eq!(JsonFileStore::new(path).load(), State::default());
    }

    #[test]
    fn reads_the_documented_format() {
        let json = r#"{
          "version": 1,
          "sources": {
            "github": { "login": "VillegasMich", "last_success": "2026-10-04T14:02:11Z" },
            "gitlab:gitlab.com": { "login": "villegasmich", "last_success": "2026-10-04T14:02:12Z" }
          },
          "seen": { "github:2874651234": "2026-10-04T13:58:40Z" },
          "last_played": [
            { "key": "github:2874651234", "repo": "acme/api", "number": 412,
              "author": "VillegasMich", "tag": "default.wav", "played_at": "2026-10-04T14:00:05Z" }
          ]
        }"#;
        let state: State = serde_json::from_str(json).unwrap();
        assert_eq!(state.sources.len(), 2);
        assert_eq!(state.last_played[0].number, 412);
    }

    #[test]
    fn mark_seen_reports_new_keys_only() {
        let mut state = State::default();
        assert!(state.mark_seen("github:1", at("2026-10-04T13:00:00Z")));
        assert!(!state.mark_seen("github:1", at("2026-10-04T13:00:00Z")));
    }

    #[test]
    fn prune_drops_old_entries() {
        let mut state = State::default();
        state.mark_seen("old", at("2026-10-01T00:00:00Z"));
        state.mark_seen("new", at("2026-10-04T13:00:00Z"));
        state.prune(at("2026-10-04T14:00:00Z"), Duration::from_secs(86_400));
        assert_eq!(state.seen.keys().collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn last_played_is_bounded_newest_first() {
        let mut state = State::default();
        for n in 0..25 {
            state.record_play(PlayRecord {
                key: format!("github:{n}"),
                repo: "a/b".into(),
                number: n,
                author: "x".into(),
                tag: "default.wav".into(),
                played_at: at("2026-10-04T14:00:00Z"),
            });
        }
        assert_eq!(state.last_played.len(), LAST_PLAYED_MAX);
        assert_eq!(state.last_played[0].number, 24);
    }
}
