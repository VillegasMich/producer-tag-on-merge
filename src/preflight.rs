//! Startup checks (also the `check` command): accounts, watched repos, player, tags, audio.

use std::fmt;
use std::time::Duration;

use tracing::{error, info, warn};

use crate::config::{Config, Watch};
use crate::exec::{Cmd, find_program};
use crate::http::HttpError;
use crate::player::{Player, PlayerError};
use crate::source::MergeSource;
use crate::tags::{self, TagOwner};

const PACTL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub level: Level,
    pub what: String,
    pub detail: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = match self.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Error => "FAIL",
        };
        write!(f, "[{mark}] {:<10} {}", self.what, self.detail)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    fn push(&mut self, level: Level, what: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(Check {
            level,
            what: what.into(),
            detail: detail.into(),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.checks.iter().any(|c| c.level == Level::Error)
    }

    /// Logs every check at its level (daemon / once).
    pub fn log(&self) {
        for c in &self.checks {
            match c.level {
                Level::Ok => info!(check = c.what, "{}", c.detail),
                Level::Warn => warn!(check = c.what, "{}", c.detail),
                Level::Error => error!(check = c.what, "{}", c.detail),
            }
        }
    }
}

/// What to check besides the accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Player, tags and audio server. Off for `once --dry-run`, which plays nothing.
    pub audio: bool,
}

pub fn run(
    config: &Config,
    sources: &mut [Box<dyn MergeSource>],
    player: &Result<Box<dyn Player>, PlayerError>,
    options: Options,
) -> Report {
    let mut report = Report::default();
    check_sources(config, sources, &mut report);
    if options.audio {
        check_audio(config, player, &mut report);
    }
    report
}

fn check_sources(config: &Config, sources: &mut [Box<dyn MergeSource>], report: &mut Report) {
    if sources.is_empty() {
        report.push(
            Level::Error,
            "accounts",
            "no account linked: set GITHUB_TOKEN and/or GITLAB_TOKEN",
        );
        return;
    }
    for source in sources.iter_mut() {
        let id = source.id().to_owned();
        match source.login() {
            Ok(login) => report.push(
                Level::Ok,
                &id,
                format!("authenticated as {login} (WATCH={})", config.watch),
            ),
            Err(e @ HttpError::Unauthorized { .. }) => {
                report.push(Level::Error, &id, e.to_string());
                continue;
            }
            Err(e) => {
                report.push(
                    Level::Warn,
                    &id,
                    format!("{e}; will keep retrying in the background"),
                );
                continue;
            }
        }
        if config.watch == Watch::Repos {
            for (entry, result) in source.check_targets() {
                match result {
                    Ok(()) => report.push(Level::Ok, &id, format!("can read {entry}")),
                    Err(e @ (HttpError::NotFound | HttpError::Unauthorized { .. })) => {
                        report.push(Level::Error, &id, format!("{entry}: {e}"));
                    }
                    Err(e) => report.push(Level::Warn, &id, format!("{entry}: {e}")),
                }
            }
        }
    }
}

fn check_audio(
    config: &Config,
    player: &Result<Box<dyn Player>, PlayerError>,
    report: &mut Report,
) {
    let player = match player {
        Ok(player) => player,
        Err(e) => {
            report.push(Level::Error, "player", e.to_string());
            return;
        }
    };
    let silent = match player.program() {
        None => {
            report.push(
                Level::Ok,
                "player",
                "PLAYER=none: tags are logged, not played",
            );
            true
        }
        Some(program) => {
            match find_program(program) {
                Some(path) => report.push(Level::Ok, "player", path.display().to_string()),
                None => report.push(
                    Level::Error,
                    "player",
                    format!("`{program}` not found on PATH (PLAYER={})", config.player),
                ),
            }
            false
        }
    };

    let dir = &config.tags_dir;
    match tags::find(dir, &TagOwner::Default) {
        Some(tag) => report.push(Level::Ok, "tags", format!("default tag {}", tag.display())),
        None => report.push(
            if silent { Level::Warn } else { Level::Error },
            "tags",
            format!(
                "no default.{{{}}} in {}; add yours with `producer-tag-on-merge tag set <file>`",
                tags::EXTENSIONS.join(","),
                dir.display()
            ),
        ),
    }

    if !silent && player.uses_pulse() && find_program("pactl").is_some() {
        match Cmd::new("pactl").arg("info").timeout(PACTL_TIMEOUT).run() {
            Ok(info) => {
                let server = info
                    .lines()
                    .find_map(|l| l.strip_prefix("Server Name:"))
                    .map(str::trim)
                    .unwrap_or("reachable");
                report.push(Level::Ok, "audio", format!("sound server: {server}"));
            }
            Err(e) => report.push(
                Level::Warn,
                "audio",
                format!(
                    "sound server not reachable ({e}); tags won't be heard until it is \
                     (logged in to a desktop session? socket mounted in Docker?)"
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Config;
    use crate::player::NullPlayer;
    use crate::source::Platform;
    use crate::source::fake::ScriptedSource;

    fn config(tags_dir: PathBuf) -> Config {
        let tags = tags_dir.display().to_string();
        Config::from_lookup(|k| match k {
            "DATA_DIR" => Some("/tmp/ptom".to_owned()),
            "TAGS_DIR" => Some(tags.clone()),
            "PLAYER" => Some("none".to_owned()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn no_sources_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(
            &config(dir.path().to_owned()),
            &mut [],
            &Ok(Box::new(NullPlayer)),
            Options { audio: false },
        );
        assert!(report.has_errors());
    }

    #[test]
    fn silent_player_only_warns_about_missing_tag() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources: Vec<Box<dyn MergeSource>> =
            vec![Box::new(ScriptedSource::new("github", Platform::GitHub))];
        let report = run(
            &config(dir.path().to_owned()),
            &mut sources,
            &Ok(Box::new(NullPlayer)),
            Options { audio: true },
        );
        assert!(!report.has_errors(), "{report:?}");
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.what == "tags" && c.level == Level::Warn)
        );
        assert!(report.checks[0].detail.contains("authenticated as you"));
    }

    #[test]
    fn missing_player_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources: Vec<Box<dyn MergeSource>> =
            vec![Box::new(ScriptedSource::new("github", Platform::GitHub))];
        let report = run(
            &config(dir.path().to_owned()),
            &mut sources,
            &Err(PlayerError::NoneFound),
            Options { audio: true },
        );
        assert!(report.has_errors());
    }
}
