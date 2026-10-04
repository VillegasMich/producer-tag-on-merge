//! Configuration loaded from environment variables (optionally via `--env-file`).
//! See `docs/configuration.md`.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono_tz::Tz;

use crate::hours::QuietHours;

pub const APP_NAME: &str = "producer-tag-on-merge";

pub const DEFAULT_GITHUB_API_URL: &str = "https://api.github.com";
pub const DEFAULT_GITLAB_URL: &str = "https://gitlab.com";
pub const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 60;
pub const DEFAULT_CATCH_UP_MINUTES: u64 = 30;
pub const DEFAULT_MAX_PLAYS_PER_POLL: usize = 3;
pub const DEFAULT_VOLUME: u8 = 100;
pub const DEFAULT_PLAY_TIMEOUT_SECONDS: u64 = 15;

/// A value that must never be logged: `Debug` and `Display` print a placeholder.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Which merges play a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    /// PRs/MRs authored by the token's user, in any repository.
    Mine,
    /// Every PR/MR merged in `GITHUB_REPOS` / `GITLAB_PROJECTS`.
    Repos,
}

impl fmt::Display for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Mine => "mine",
            Self::Repos => "repos",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubConfig {
    pub token: Secret,
    /// Without trailing slash.
    pub api_url: String,
    /// `owner/repo` or `owner` entries; only used with `WATCH=repos`.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLabConfig {
    pub token: Secret,
    /// Instance base URL without trailing slash, e.g. `https://gitlab.com`.
    pub url: String,
    /// Project or group paths; only used with `WATCH=repos`.
    pub projects: Vec<String>,
}

impl GitLabConfig {
    /// `host[:port]` of the instance, part of the state key and event keys.
    pub fn host(&self) -> &str {
        url_authority(&self.url)
    }
}

/// `PLAYER` setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerSetting {
    Auto,
    Paplay,
    PwPlay,
    Aplay,
    Afplay,
    /// `PLAYER_COMMAND`, split into argv; contains a `{file}` placeholder.
    Command(Vec<String>),
    None,
}

impl fmt::Display for PlayerSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Paplay => "paplay",
            Self::PwPlay => "pw-play",
            Self::Aplay => "aplay",
            Self::Afplay => "afplay",
            Self::Command(_) => "command",
            Self::None => "none",
        })
    }
}

/// Validated service configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub github: Option<GitHubConfig>,
    pub gitlab: Option<GitLabConfig>,
    pub watch: Watch,
    pub poll_interval: Duration,
    pub catch_up: Duration,
    pub max_plays_per_poll: usize,
    pub tags_dir: PathBuf,
    pub player: PlayerSetting,
    pub volume: u8,
    pub play_timeout: Duration,
    pub quiet_hours: Option<QuietHours>,
    pub timezone: Tz,
    pub data_dir: PathBuf,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("no account linked: set GITHUB_TOKEN and/or GITLAB_TOKEN (see docs/configuration.md)")]
    NoSource,
    #[error("{var}={value:?} is invalid: {reason}")]
    Invalid {
        var: &'static str,
        value: String,
        reason: String,
    },
    #[error("{var} is required: {reason}")]
    Missing {
        var: &'static str,
        reason: &'static str,
    },
    #[error("cannot determine the default {what} directory; set {var}")]
    NoDefaultDir {
        what: &'static str,
        var: &'static str,
    },
}

impl Config {
    /// Builds the config from a key lookup (real environment + env file), so validation is
    /// testable without touching the process environment. Does not require a linked account:
    /// `simulate`, `play` and `tag` work without one; see [`Config::require_source`].
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let watch = match get("WATCH").as_deref() {
            None | Some("mine") => Watch::Mine,
            Some("repos") => Watch::Repos,
            Some(other) => return Err(invalid("WATCH", other, "expected `mine` or `repos`")),
        };

        let github = match get("GITHUB_TOKEN") {
            None => None,
            Some(token) => Some(GitHubConfig {
                token: Secret::new(token),
                api_url: parse_url(
                    "GITHUB_API_URL",
                    get("GITHUB_API_URL")
                        .as_deref()
                        .unwrap_or(DEFAULT_GITHUB_API_URL),
                )?,
                repos: parse_list("GITHUB_REPOS", get("GITHUB_REPOS"), validate_github_repo)?,
            }),
        };
        let gitlab = match get("GITLAB_TOKEN") {
            None => None,
            Some(token) => Some(GitLabConfig {
                token: Secret::new(token),
                url: parse_url(
                    "GITLAB_URL",
                    get("GITLAB_URL").as_deref().unwrap_or(DEFAULT_GITLAB_URL),
                )?,
                projects: parse_list(
                    "GITLAB_PROJECTS",
                    get("GITLAB_PROJECTS"),
                    validate_gitlab_path,
                )?,
            }),
        };
        if watch == Watch::Repos {
            if github.as_ref().is_some_and(|g| g.repos.is_empty()) {
                return Err(ConfigError::Missing {
                    var: "GITHUB_REPOS",
                    reason: "WATCH=repos needs the repos/owners to watch for the linked GitHub account",
                });
            }
            if gitlab.as_ref().is_some_and(|g| g.projects.is_empty()) {
                return Err(ConfigError::Missing {
                    var: "GITLAB_PROJECTS",
                    reason: "WATCH=repos needs the projects/groups to watch for the linked GitLab account",
                });
            }
        }

        let poll_interval = Duration::from_secs(parse_number(
            "POLL_INTERVAL_SECONDS",
            get("POLL_INTERVAL_SECONDS"),
            DEFAULT_POLL_INTERVAL_SECONDS,
            15..=3600,
        )?);
        let catch_up = Duration::from_secs(
            60 * parse_number(
                "CATCH_UP_MINUTES",
                get("CATCH_UP_MINUTES"),
                DEFAULT_CATCH_UP_MINUTES,
                0..=10_080,
            )?,
        );
        let max_plays_per_poll = parse_number(
            "MAX_PLAYS_PER_POLL",
            get("MAX_PLAYS_PER_POLL"),
            DEFAULT_MAX_PLAYS_PER_POLL as u64,
            1..=20,
        )? as usize;
        let volume = parse_number("VOLUME", get("VOLUME"), DEFAULT_VOLUME.into(), 0..=100)? as u8;
        let play_timeout = Duration::from_secs(parse_number(
            "PLAY_TIMEOUT_SECONDS",
            get("PLAY_TIMEOUT_SECONDS"),
            DEFAULT_PLAY_TIMEOUT_SECONDS,
            1..=300,
        )?);

        let player = parse_player(get("PLAYER"), get("PLAYER_COMMAND"))?;

        let quiet_hours = match get("QUIET_HOURS") {
            None => None,
            Some(v) => Some(
                v.parse::<QuietHours>()
                    .map_err(|e| invalid("QUIET_HOURS", &v, e.to_string()))?,
            ),
        };
        let timezone = match get("TIMEZONE") {
            None => Tz::UTC,
            Some(v) => v.parse::<Tz>().map_err(|_| {
                invalid("TIMEZONE", &v, "expected an IANA name like America/Bogota")
            })?,
        };

        let data_dir = match get("DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => default_data_dir()?,
        };
        let tags_dir = match get("TAGS_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => default_tags_dir()?,
        };

        Ok(Self {
            github,
            gitlab,
            watch,
            poll_interval,
            catch_up,
            max_plays_per_poll,
            tags_dir,
            player,
            volume,
            play_timeout,
            quiet_hours,
            timezone,
            data_dir,
        })
    }

    /// Commands that talk to the forges need at least one linked account.
    pub fn require_source(&self) -> Result<(), ConfigError> {
        if self.github.is_none() && self.gitlab.is_none() {
            return Err(ConfigError::NoSource);
        }
        Ok(())
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }
}

/// Native default: `$XDG_DATA_HOME/producer-tag-on-merge` on Linux,
/// `~/Library/Application Support/producer-tag-on-merge` on macOS. The Docker image sets `/data`.
fn default_data_dir() -> Result<PathBuf, ConfigError> {
    dirs::data_dir()
        .map(|d| d.join(APP_NAME))
        .ok_or(ConfigError::NoDefaultDir {
            what: "data",
            var: "DATA_DIR",
        })
}

/// Native default: `~/.config/producer-tag-on-merge/tags` on both platforms, next to the env
/// file, where `scripts/install.sh` and the service units expect it. The Docker image sets `/tags`.
fn default_tags_dir() -> Result<PathBuf, ConfigError> {
    dirs::home_dir()
        .map(|home| home.join(".config").join(APP_NAME).join("tags"))
        .ok_or(ConfigError::NoDefaultDir {
            what: "tags",
            var: "TAGS_DIR",
        })
}

fn invalid(var: &'static str, value: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        value: value.to_owned(),
        reason: reason.into(),
    }
}

fn parse_number(
    var: &'static str,
    value: Option<String>,
    default: u64,
    range: std::ops::RangeInclusive<u64>,
) -> Result<u64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let reason = || {
        format!(
            "expected a whole number in {}-{}",
            range.start(),
            range.end()
        )
    };
    let n: u64 = value.parse().map_err(|_| invalid(var, &value, reason()))?;
    if !range.contains(&n) {
        return Err(invalid(var, &value, reason()));
    }
    Ok(n)
}

fn parse_url(var: &'static str, value: &str) -> Result<String, ConfigError> {
    let url = value.trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| invalid(var, value, "expected an http(s):// URL"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains(['@', ' ', '?', '#']) {
        return Err(invalid(
            var,
            value,
            "expected an http(s):// URL with a host",
        ));
    }
    Ok(url.to_owned())
}

/// `host[:port]` of a URL already validated by [`parse_url`].
fn url_authority(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

fn parse_list(
    var: &'static str,
    value: Option<String>,
    validate: fn(&str) -> bool,
) -> Result<Vec<String>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut entries: Vec<String> = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        if !validate(entry) {
            return Err(invalid(var, entry, "not a valid repository/owner path"));
        }
        if !entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
            entries.push(entry.to_owned());
        }
    }
    Ok(entries)
}

fn is_path_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `owner` or `owner/repo`. Also keeps the search query free of spaces and operators.
fn validate_github_repo(entry: &str) -> bool {
    let mut parts = entry.split('/');
    let valid = parts.next().is_some_and(is_path_segment);
    match (parts.next(), parts.next()) {
        (None, None) => valid,
        (Some(repo), None) => valid && is_path_segment(repo),
        _ => false,
    }
}

/// `group`, `group/project`, `group/sub/project`...
fn validate_gitlab_path(entry: &str) -> bool {
    entry.split('/').all(is_path_segment)
}

fn parse_player(
    player: Option<String>,
    command: Option<String>,
) -> Result<PlayerSetting, ConfigError> {
    Ok(match player.as_deref().unwrap_or("auto") {
        "auto" => PlayerSetting::Auto,
        "paplay" => PlayerSetting::Paplay,
        "pw-play" => PlayerSetting::PwPlay,
        "aplay" => PlayerSetting::Aplay,
        "afplay" => PlayerSetting::Afplay,
        "none" => PlayerSetting::None,
        "command" => {
            let command = command.ok_or(ConfigError::Missing {
                var: "PLAYER_COMMAND",
                reason: "PLAYER=command needs the program to run, e.g. `ffplay -nodisp -autoexit {file}`",
            })?;
            let argv: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
            if !argv.iter().skip(1).any(|a| a.contains("{file}")) {
                return Err(invalid(
                    "PLAYER_COMMAND",
                    &command,
                    "must contain the {file} placeholder as an argument",
                ));
            }
            PlayerSetting::Command(argv)
        }
        other => {
            return Err(invalid(
                "PLAYER",
                other,
                "expected auto, paplay, pw-play, aplay, afplay, command or none",
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut map: HashMap<String, String> = HashMap::from([
            ("DATA_DIR".to_owned(), "/tmp/ptom-data".to_owned()),
            ("TAGS_DIR".to_owned(), "/tmp/ptom-tags".to_owned()),
        ]);
        for (k, v) in vars {
            map.insert((*k).to_owned(), (*v).to_owned());
        }
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let c = config(&[("GITHUB_TOKEN", "ghp_x")]).unwrap();
        let gh = c.github.as_ref().unwrap();
        assert_eq!(gh.api_url, DEFAULT_GITHUB_API_URL);
        assert!(c.gitlab.is_none());
        assert_eq!(c.watch, Watch::Mine);
        assert_eq!(c.poll_interval, Duration::from_secs(60));
        assert_eq!(c.catch_up, Duration::from_secs(30 * 60));
        assert_eq!(c.max_plays_per_poll, 3);
        assert_eq!(c.player, PlayerSetting::Auto);
        assert_eq!(c.volume, 100);
        assert_eq!(c.play_timeout, Duration::from_secs(15));
        assert_eq!(c.quiet_hours, None);
        assert_eq!(c.timezone, Tz::UTC);
        assert_eq!(c.state_file(), PathBuf::from("/tmp/ptom-data/state.json"));
        c.require_source().unwrap();
    }

    #[test]
    fn no_token_is_allowed_until_required() {
        let c = config(&[("GITHUB_TOKEN", "  ")]).unwrap();
        assert_eq!(c.require_source(), Err(ConfigError::NoSource));
    }

    #[test]
    fn token_is_redacted_in_debug_output() {
        let c = config(&[
            ("GITHUB_TOKEN", "ghp_supersecret"),
            ("GITLAB_TOKEN", "glpat-secret"),
        ])
        .unwrap();
        let debug = format!("{c:?}");
        assert!(!debug.contains("supersecret"), "{debug}");
        assert!(!debug.contains("glpat"), "{debug}");
        assert_eq!(c.github.unwrap().token.to_string(), "[redacted]");
    }

    #[test]
    fn gitlab_url_and_host() {
        let c = config(&[
            ("GITLAB_TOKEN", "glpat"),
            ("GITLAB_URL", "https://gitlab.example.com:8443/"),
        ])
        .unwrap();
        let gl = c.gitlab.unwrap();
        assert_eq!(gl.url, "https://gitlab.example.com:8443");
        assert_eq!(gl.host(), "gitlab.example.com:8443");
        assert!(config(&[("GITLAB_TOKEN", "x"), ("GITLAB_URL", "gitlab.com")]).is_err());
        assert!(config(&[("GITLAB_TOKEN", "x"), ("GITLAB_URL", "https://")]).is_err());
    }

    #[test]
    fn repos_mode_requires_lists() {
        let err = config(&[("GITHUB_TOKEN", "x"), ("WATCH", "repos")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "GITHUB_REPOS",
                ..
            }
        ));

        let c = config(&[
            ("GITHUB_TOKEN", "x"),
            ("WATCH", "repos"),
            (
                "GITHUB_REPOS",
                " acme/api, acme/web ,VillegasMich,,acme/API",
            ),
        ])
        .unwrap();
        assert_eq!(
            c.github.unwrap().repos,
            ["acme/api", "acme/web", "VillegasMich"]
        );

        let c = config(&[
            ("GITLAB_TOKEN", "x"),
            ("WATCH", "repos"),
            ("GITLAB_PROJECTS", "acme/backend/api,acme"),
        ])
        .unwrap();
        assert_eq!(c.gitlab.unwrap().projects, ["acme/backend/api", "acme"]);
    }

    #[test]
    fn rejects_unsafe_repo_entries() {
        for bad in ["a/b/c", "a b", "../x", "a/..", "repo:x", "/a"] {
            assert!(
                config(&[
                    ("GITHUB_TOKEN", "x"),
                    ("WATCH", "repos"),
                    ("GITHUB_REPOS", bad)
                ])
                .is_err(),
                "{bad}"
            );
        }
        for bad in ["a//b", "a/../b", "a b"] {
            assert!(
                config(&[
                    ("GITLAB_TOKEN", "x"),
                    ("WATCH", "repos"),
                    ("GITLAB_PROJECTS", bad)
                ])
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn numeric_bounds() {
        assert!(config(&[("POLL_INTERVAL_SECONDS", "14")]).is_err());
        assert!(config(&[("POLL_INTERVAL_SECONDS", "3601")]).is_err());
        assert!(config(&[("POLL_INTERVAL_SECONDS", "15")]).is_ok());
        assert!(config(&[("CATCH_UP_MINUTES", "0")]).is_ok());
        assert!(config(&[("MAX_PLAYS_PER_POLL", "0")]).is_err());
        assert!(config(&[("VOLUME", "101")]).is_err());
        assert!(config(&[("VOLUME", "-1")]).is_err());
        assert!(config(&[("PLAY_TIMEOUT_SECONDS", "abc")]).is_err());
    }

    #[test]
    fn player_settings() {
        assert_eq!(
            config(&[("PLAYER", "pw-play")]).unwrap().player,
            PlayerSetting::PwPlay
        );
        assert_eq!(
            config(&[("PLAYER", "none")]).unwrap().player,
            PlayerSetting::None
        );
        assert!(config(&[("PLAYER", "vlc")]).is_err());
        assert!(matches!(
            config(&[("PLAYER", "command")]).unwrap_err(),
            ConfigError::Missing {
                var: "PLAYER_COMMAND",
                ..
            }
        ));
        assert!(config(&[("PLAYER", "command"), ("PLAYER_COMMAND", "ffplay")]).is_err());
        let c = config(&[
            ("PLAYER", "command"),
            ("PLAYER_COMMAND", "ffplay -nodisp -autoexit   {file}"),
        ])
        .unwrap();
        assert_eq!(
            c.player,
            PlayerSetting::Command(vec![
                "ffplay".into(),
                "-nodisp".into(),
                "-autoexit".into(),
                "{file}".into()
            ])
        );
    }

    #[test]
    fn quiet_hours_and_timezone() {
        let c = config(&[
            ("QUIET_HOURS", "22:00-08:00"),
            ("TIMEZONE", "America/Bogota"),
        ])
        .unwrap();
        assert_eq!(c.quiet_hours.unwrap().to_string(), "22:00-08:00");
        assert_eq!(c.timezone, chrono_tz::America::Bogota);
        assert!(config(&[("QUIET_HOURS", "late")]).is_err());
        assert!(config(&[("TIMEZONE", "Mars/Olympus")]).is_err());
    }

    #[test]
    fn invalid_watch() {
        assert!(config(&[("WATCH", "all")]).is_err());
    }
}
