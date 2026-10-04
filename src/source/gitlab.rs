//! GitLab (gitlab.com or self-hosted) through the merge requests API.

use chrono::{DateTime, Utc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use tracing::warn;

use super::{MergeEvent, MergeSource, Platform, iso8601, parse_lenient};
use crate::config::{GitLabConfig, Secret, Watch};
use crate::http::{Fetched, HttpClient, HttpError};

const PER_PAGE: u32 = 50;

/// Path segment encoding: `acme/backend/api` → `acme%2Fbackend%2Fapi`.
const PATH: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.');

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Project,
    Group,
}

#[derive(Debug, Clone)]
struct Target {
    path: String,
    /// Resolved on first use: a path can name a project or a group.
    kind: Option<TargetKind>,
}

pub struct GitLabSource {
    http: HttpClient,
    token: Secret,
    api: String,
    host: String,
    id: String,
    watch: Watch,
    targets: Vec<Target>,
    login: Option<String>,
}

/// State key of a GitLab instance.
pub fn source_id(host: &str) -> String {
    format!("gitlab:{host}")
}

impl GitLabSource {
    pub fn new(config: &GitLabConfig, watch: Watch) -> Self {
        Self {
            http: HttpClient::new(),
            token: config.token.clone(),
            api: format!("{}/api/v4", config.url),
            host: config.host().to_owned(),
            id: source_id(config.host()),
            watch,
            targets: config
                .projects
                .iter()
                .map(|path| Target {
                    path: path.clone(),
                    kind: None,
                })
                .collect(),
            login: None,
        }
    }

    fn get(&mut self, path: &str, conditional: bool) -> Result<Fetched, HttpError> {
        let url = format!("{}{path}", self.api);
        self.http
            .get(&url, &[("PRIVATE-TOKEN", self.token.expose())], conditional)
    }

    /// Project first, then group.
    fn resolve(&mut self, path: &str) -> Result<TargetKind, HttpError> {
        let encoded = encode_path(path);
        match self.get(&format!("/projects/{encoded}"), false) {
            Ok(_) => Ok(TargetKind::Project),
            Err(HttpError::NotFound) => self
                .get(&format!("/groups/{encoded}"), false)
                .map(|_| TargetKind::Group),
            Err(e) => Err(e),
        }
    }

    fn fetch_mrs(
        &mut self,
        path: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<MergeEvent>, HttpError> {
        match self.get(path, true)? {
            Fetched::Body(body) => parse_merge_requests(&body, &self.host, since),
            Fetched::NotModified => Ok(Vec::new()),
        }
    }
}

impl MergeSource for GitLabSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn platform(&self) -> Platform {
        Platform::GitLab
    }

    fn login(&mut self) -> Result<String, HttpError> {
        if let Some(login) = &self.login {
            return Ok(login.clone());
        }
        #[derive(Deserialize)]
        struct User {
            username: String,
        }
        let body = match self.get("/user", false)? {
            Fetched::Body(body) => body,
            Fetched::NotModified => return Err(HttpError::Parse("unexpected 304".to_owned())),
        };
        let user: User =
            serde_json::from_str(&body).map_err(|e| HttpError::Parse(format!("GET /user: {e}")))?;
        self.login = Some(user.username.clone());
        Ok(user.username)
    }

    fn cached_login(&self) -> Option<&str> {
        self.login.as_deref()
    }

    fn check_targets(&mut self) -> Vec<(String, Result<(), HttpError>)> {
        if self.watch != Watch::Repos {
            return Vec::new();
        }
        (0..self.targets.len())
            .map(|i| {
                let path = self.targets[i].path.clone();
                let result = self.resolve(&path).map(|kind| {
                    self.targets[i].kind = Some(kind);
                });
                (path, result)
            })
            .collect()
    }

    fn poll(&mut self, since: DateTime<Utc>) -> Result<Vec<MergeEvent>, HttpError> {
        let query = mr_query(since);
        match self.watch {
            Watch::Mine => {
                // Authenticate first so a bad token is reported as such.
                self.login()?;
                self.fetch_mrs(
                    &format!("/merge_requests?scope=created_by_me&{query}"),
                    since,
                )
            }
            Watch::Repos => {
                let mut events = Vec::new();
                for i in 0..self.targets.len() {
                    let path = self.targets[i].path.clone();
                    let kind = match self.targets[i].kind {
                        Some(kind) => kind,
                        None => match self.resolve(&path) {
                            Ok(kind) => {
                                self.targets[i].kind = Some(kind);
                                kind
                            }
                            Err(HttpError::NotFound) => {
                                warn!(
                                    project = path,
                                    "GitLab project/group not found; skipping it"
                                );
                                continue;
                            }
                            Err(e) => return Err(e),
                        },
                    };
                    let scope = match kind {
                        TargetKind::Project => "projects",
                        TargetKind::Group => "groups",
                    };
                    let url = format!("/{scope}/{}/merge_requests?{query}", encode_path(&path));
                    for event in self.fetch_mrs(&url, since)? {
                        if !events.iter().any(|e: &MergeEvent| e.key == event.key) {
                            events.push(event);
                        }
                    }
                }
                Ok(events)
            }
        }
    }
}

fn encode_path(path: &str) -> String {
    utf8_percent_encode(path, PATH).to_string()
}

/// Query string shared by the user, project and group MR listings.
pub fn mr_query(since: DateTime<Utc>) -> String {
    format!(
        "state=merged&updated_after={}&order_by=updated_at&sort=desc&per_page={PER_PAGE}",
        utf8_percent_encode(&iso8601(since), NON_ALPHANUMERIC)
    )
}

#[derive(Deserialize)]
struct MergeRequest {
    id: u64,
    iid: u64,
    title: String,
    web_url: String,
    author: Author,
    merged_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    references: Option<References>,
}

#[derive(Deserialize)]
struct Author {
    username: String,
}

#[derive(Deserialize)]
struct References {
    full: Option<String>,
}

/// Parses a merge request list. `updated_after` returns a superset, so MRs merged before `since`
/// are dropped (`updated_at` stands in for `merged_at` on instances that don't report it).
pub fn parse_merge_requests(
    body: &str,
    host: &str,
    since: DateTime<Utc>,
) -> Result<Vec<MergeEvent>, HttpError> {
    let items: Vec<serde_json::Value> = serde_json::from_str(body)
        .map_err(|e| HttpError::Parse(format!("GitLab merge requests: {e}")))?;
    Ok(parse_lenient::<MergeRequest>(items, "GitLab merge request")
        .into_iter()
        .filter_map(|mr| {
            let merged_at = mr.merged_at.or(mr.updated_at)?;
            if merged_at < since {
                return None;
            }
            let repo = mr
                .references
                .and_then(|r| r.full)
                .and_then(|full| full.split_once('!').map(|(repo, _)| repo.to_owned()))
                .or_else(|| repo_from_web_url(&mr.web_url))?;
            Some(MergeEvent {
                key: format!("gitlab:{host}:{}", mr.id),
                platform: Platform::GitLab,
                repo,
                number: mr.iid,
                title: mr.title,
                author: mr.author.username,
                merged_at,
                url: mr.web_url,
            })
        })
        .collect())
}

/// `https://host/group/sub/project/-/merge_requests/3` → `group/sub/project`.
fn repo_from_web_url(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let path = rest.split_once('/')?.1;
    let repo = path.split_once("/-/")?.0;
    (!repo.is_empty()).then(|| repo.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    #[test]
    fn query_and_paths() {
        assert_eq!(
            mr_query(at("2026-10-04T13:00:00Z")),
            "state=merged&updated_after=2026%2D10%2D04T13%3A00%3A00Z&order_by=updated_at&sort=desc&per_page=50"
        );
        assert_eq!(
            encode_path("acme/backend/my-api_v2.x"),
            "acme%2Fbackend%2Fmy-api_v2.x"
        );
        assert_eq!(source_id("gitlab.com"), "gitlab:gitlab.com");
    }

    #[test]
    fn parses_fixture() {
        let body = include_str!("../../tests/fixtures/gitlab_merge_requests.json");
        let events = parse_merge_requests(body, "gitlab.com", at("2026-10-04T11:00:00Z")).unwrap();
        assert_eq!(events.len(), 2, "old and incomplete MRs are skipped");

        let first = &events[0];
        assert_eq!(first.key, "gitlab:gitlab.com:331245");
        assert_eq!(first.platform, Platform::GitLab);
        assert_eq!(first.repo, "acme/backend/api");
        assert_eq!(first.number, 57);
        assert_eq!(first.author, "JDoe");
        assert_eq!(first.merged_at, at("2026-10-04T11:20:03.120Z"));
        assert_eq!(first.to_string(), "acme/backend/api!57 by JDoe");

        // No merged_at: updated_at is used; no references: the repo comes from web_url.
        let second = &events[1];
        assert_eq!(second.repo, "acme/frontend");
        assert_eq!(second.merged_at, at("2026-10-04T11:30:00Z"));
    }

    #[test]
    fn rejects_non_list_bodies() {
        assert!(parse_merge_requests("{}", "h", at("2026-10-04T11:00:00Z")).is_err());
        assert_eq!(
            parse_merge_requests("[]", "h", at("2026-10-04T11:00:00Z")).unwrap(),
            []
        );
    }

    #[test]
    fn repo_from_urls() {
        assert_eq!(
            repo_from_web_url("https://gitlab.example.com:8443/a/b/c/-/merge_requests/1")
                .as_deref(),
            Some("a/b/c")
        );
        assert_eq!(
            repo_from_web_url("https://gitlab.com/-/merge_requests/1"),
            None
        );
        assert_eq!(repo_from_web_url("nonsense"), None);
    }
}
