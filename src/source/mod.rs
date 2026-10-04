//! Merge sources: where merge events come from (GitHub, GitLab, or a fake for `simulate`).

pub mod fake;
pub mod github;
pub mod gitlab;

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::http::HttpError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    GitHub,
    GitLab,
}

impl Platform {
    pub const ALL: [Self; 2] = [Self::GitHub, Self::GitLab];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Platform {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "github" => Ok(Self::GitHub),
            "gitlab" => Ok(Self::GitLab),
            other => Err(format!(
                "unknown platform `{other}` (expected github or gitlab)"
            )),
        }
    }
}

/// A PR/MR that was merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEvent {
    /// Unique and stable: `github:<id>`, `gitlab:<host>:<id>`.
    pub key: String,
    pub platform: Platform,
    /// `owner/repo` or `group/sub/project`.
    pub repo: String,
    /// PR number / MR iid.
    pub number: u64,
    pub title: String,
    /// Login / username, as returned by the API.
    pub author: String,
    pub merged_at: DateTime<Utc>,
    pub url: String,
}

impl fmt::Display for MergeEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = match self.platform {
            Platform::GitHub => '#',
            Platform::GitLab => '!',
        };
        write!(f, "{}{sep}{} by {}", self.repo, self.number, self.author)
    }
}

/// One linked account. Implementations only ever issue `GET` requests.
pub trait MergeSource {
    /// Stable key of this source in the state file: `github`, `gitlab:<host>`.
    fn id(&self) -> &str;

    fn platform(&self) -> Platform;

    /// Authenticates (`GET /user`) and returns the account's login. Cached after success.
    fn login(&mut self) -> Result<String, HttpError>;

    /// The login, if [`MergeSource::login`] already succeeded.
    fn cached_login(&self) -> Option<&str>;

    /// With `WATCH=repos`: checks that every watched repo/project/owner is readable.
    /// Returns one result per entry. Empty in `WATCH=mine`.
    fn check_targets(&mut self) -> Vec<(String, Result<(), HttpError>)>;

    /// Merges with `merged_at >= since` (or a superset; the poller dedupes).
    fn poll(&mut self, since: DateTime<Utc>) -> Result<Vec<MergeEvent>, HttpError>;
}

/// The real sources for the linked accounts.
pub fn from_config(config: &Config) -> Vec<Box<dyn MergeSource>> {
    let mut sources: Vec<Box<dyn MergeSource>> = Vec::new();
    if let Some(github) = &config.github {
        sources.push(Box::new(github::GitHubSource::new(github, config.watch)));
    }
    if let Some(gitlab) = &config.gitlab {
        sources.push(Box::new(gitlab::GitLabSource::new(gitlab, config.watch)));
    }
    sources
}

/// ISO 8601 UTC with seconds, the format both APIs accept in queries.
pub fn iso8601(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parses each element of a JSON array on its own, skipping (and logging) elements that don't
/// match `T`, so one odd item can't hide the others.
pub(crate) fn parse_lenient<T: serde::de::DeserializeOwned>(
    items: Vec<serde_json::Value>,
    what: &str,
) -> Vec<T> {
    items
        .into_iter()
        .filter_map(|item| match serde_json::from_value(item) {
            Ok(parsed) => Some(parsed),
            Err(e) => {
                tracing::debug!(what, error = %e, "skipping item with unexpected shape");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_round_trip() {
        for p in Platform::ALL {
            assert_eq!(p.as_str().parse::<Platform>().unwrap(), p);
        }
        assert_eq!("GitHub".parse::<Platform>().unwrap(), Platform::GitHub);
        assert!("bitbucket".parse::<Platform>().is_err());
    }

    #[test]
    fn iso8601_format() {
        let t = DateTime::parse_from_rfc3339("2026-10-04T13:00:00.123+00:00")
            .unwrap()
            .to_utc();
        assert_eq!(iso8601(t), "2026-10-04T13:00:00Z");
    }
}
