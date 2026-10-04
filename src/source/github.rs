//! GitHub (github.com or Enterprise Server) through the Search API.

use chrono::{DateTime, Utc};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use super::{MergeEvent, MergeSource, Platform, iso8601, parse_lenient};
use crate::config::{GitHubConfig, Secret, Watch};
use crate::http::{Fetched, HttpClient, HttpError};

/// Search allows 256 characters per query; longer `repos` lists are split.
pub const MAX_QUERY_LEN: usize = 256;
const PER_PAGE: u32 = 50;

pub struct GitHubSource {
    http: HttpClient,
    token: Secret,
    api_url: String,
    watch: Watch,
    repos: Vec<String>,
    login: Option<String>,
}

impl GitHubSource {
    pub fn new(config: &GitHubConfig, watch: Watch) -> Self {
        Self {
            http: HttpClient::new(),
            token: config.token.clone(),
            api_url: config.api_url.clone(),
            watch,
            repos: config.repos.clone(),
            login: None,
        }
    }

    fn get(&mut self, path: &str, conditional: bool) -> Result<Fetched, HttpError> {
        let url = format!("{}{path}", self.api_url);
        let auth = format!("Bearer {}", self.token.expose());
        self.http.get(
            &url,
            &[
                ("Authorization", &auth),
                ("Accept", "application/vnd.github+json"),
                ("X-GitHub-Api-Version", "2022-11-28"),
            ],
            conditional,
        )
    }

    fn get_body(&mut self, path: &str) -> Result<String, HttpError> {
        match self.get(path, false)? {
            Fetched::Body(body) => Ok(body),
            Fetched::NotModified => Err(HttpError::Parse("unexpected 304".to_owned())),
        }
    }
}

impl MergeSource for GitHubSource {
    fn id(&self) -> &str {
        "github"
    }

    fn platform(&self) -> Platform {
        Platform::GitHub
    }

    fn login(&mut self) -> Result<String, HttpError> {
        if let Some(login) = &self.login {
            return Ok(login.clone());
        }
        #[derive(Deserialize)]
        struct User {
            login: String,
        }
        let body = self.get_body("/user")?;
        let user: User =
            serde_json::from_str(&body).map_err(|e| HttpError::Parse(format!("GET /user: {e}")))?;
        self.login = Some(user.login.clone());
        Ok(user.login)
    }

    fn cached_login(&self) -> Option<&str> {
        self.login.as_deref()
    }

    fn check_targets(&mut self) -> Vec<(String, Result<(), HttpError>)> {
        if self.watch != Watch::Repos {
            return Vec::new();
        }
        self.repos
            .clone()
            .into_iter()
            .map(|entry| {
                let path = if entry.contains('/') {
                    format!("/repos/{entry}")
                } else {
                    format!("/users/{entry}")
                };
                let result = self.get(&path, false).map(|_| ());
                (entry, result)
            })
            .collect()
    }

    fn poll(&mut self, since: DateTime<Utc>) -> Result<Vec<MergeEvent>, HttpError> {
        let login = match self.watch {
            Watch::Mine => Some(self.login()?),
            Watch::Repos => None,
        };
        let mut events: Vec<MergeEvent> = Vec::new();
        for query in search_queries(self.watch, login.as_deref(), &self.repos, since) {
            let path = format!(
                "/search/issues?q={}&sort=updated&order=desc&per_page={PER_PAGE}",
                utf8_percent_encode(&query, NON_ALPHANUMERIC)
            );
            // 304: same results as the previous poll of this exact query, all already seen.
            if let Fetched::Body(body) = self.get(&path, true)? {
                for event in parse_search(&body)? {
                    if !events.iter().any(|e| e.key == event.key) {
                        events.push(event);
                    }
                }
            }
        }
        Ok(events)
    }
}

/// Search queries for one poll. `mine`: one query for the user's merged PRs. `repos`: the
/// entries as `repo:`/`user:` qualifiers (OR'd by GitHub), split so no query exceeds
/// [`MAX_QUERY_LEN`].
pub fn search_queries(
    watch: Watch,
    login: Option<&str>,
    repos: &[String],
    since: DateTime<Utc>,
) -> Vec<String> {
    let base = format!("is:pr is:merged merged:>={}", iso8601(since));
    match watch {
        Watch::Mine => vec![format!("{base} author:{}", login.unwrap_or_default())],
        Watch::Repos => {
            let mut queries = Vec::new();
            let mut current = base.clone();
            for entry in repos {
                let qualifier = if entry.contains('/') {
                    format!("repo:{entry}")
                } else {
                    format!("user:{entry}")
                };
                if current.len() > base.len() && current.len() + 1 + qualifier.len() > MAX_QUERY_LEN
                {
                    queries.push(std::mem::replace(&mut current, base.clone()));
                }
                current.push(' ');
                current.push_str(&qualifier);
            }
            if current.len() > base.len() {
                queries.push(current);
            }
            queries
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Item {
    id: u64,
    number: u64,
    title: String,
    html_url: String,
    repository_url: String,
    user: Author,
    pull_request: PullRequestRef,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct PullRequestRef {
    merged_at: Option<DateTime<Utc>>,
}

/// Parses a `/search/issues` response. Items that aren't merged PRs or lack fields are skipped.
pub fn parse_search(body: &str) -> Result<Vec<MergeEvent>, HttpError> {
    let response: SearchResponse =
        serde_json::from_str(body).map_err(|e| HttpError::Parse(format!("GitHub search: {e}")))?;
    Ok(parse_lenient::<Item>(response.items, "GitHub search item")
        .into_iter()
        .filter_map(|item| {
            let merged_at = item.pull_request.merged_at?;
            let repo = item
                .repository_url
                .split_once("/repos/")
                .map(|(_, repo)| repo.to_owned())?;
            Some(MergeEvent {
                key: format!("github:{}", item.id),
                platform: Platform::GitHub,
                repo,
                number: item.number,
                title: item.title,
                author: item.user.login,
                merged_at,
                url: item.html_url,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    #[test]
    fn mine_query() {
        let q = search_queries(
            Watch::Mine,
            Some("VillegasMich"),
            &[],
            at("2026-10-04T13:00:00Z"),
        );
        assert_eq!(
            q,
            ["is:pr is:merged merged:>=2026-10-04T13:00:00Z author:VillegasMich"]
        );
    }

    #[test]
    fn repos_query() {
        let repos = vec!["acme/api".to_owned(), "acme".to_owned()];
        let q = search_queries(Watch::Repos, None, &repos, at("2026-10-04T13:00:00Z"));
        assert_eq!(
            q,
            ["is:pr is:merged merged:>=2026-10-04T13:00:00Z repo:acme/api user:acme"]
        );
    }

    #[test]
    fn long_repo_lists_are_split() {
        let repos: Vec<String> = (0..30)
            .map(|i| format!("some-org/repository-{i:02}"))
            .collect();
        let queries = search_queries(Watch::Repos, None, &repos, at("2026-10-04T13:00:00Z"));
        assert!(queries.len() > 1);
        for q in &queries {
            assert!(q.len() <= MAX_QUERY_LEN, "{} chars: {q}", q.len());
            assert!(q.starts_with("is:pr is:merged merged:>="));
        }
        let all = queries.join(" ");
        for repo in &repos {
            assert_eq!(
                all.matches(&format!("repo:{repo} ")).count()
                    + usize::from(all.ends_with(&format!("repo:{repo}"))),
                1,
                "{repo}"
            );
        }
    }

    #[test]
    fn parses_fixture() {
        let events = parse_search(include_str!("../../tests/fixtures/github_search.json")).unwrap();
        assert_eq!(events.len(), 2, "unmerged and incomplete items are skipped");
        let first = &events[0];
        assert_eq!(first.key, "github:2874651234");
        assert_eq!(first.platform, Platform::GitHub);
        assert_eq!(first.repo, "acme/api");
        assert_eq!(first.number, 412);
        assert_eq!(first.author, "OctoCat");
        assert_eq!(first.merged_at, at("2026-10-04T13:58:40Z"));
        assert_eq!(first.url, "https://github.com/acme/api/pull/412");
        assert_eq!(first.to_string(), "acme/api#412 by OctoCat");
        assert_eq!(events[1].author, "dependabot[bot]");
    }

    #[test]
    fn rejects_non_search_bodies() {
        assert!(matches!(parse_search("{}"), Err(HttpError::Parse(_))));
        assert!(matches!(parse_search("not json"), Err(HttpError::Parse(_))));
        assert_eq!(parse_search(r#"{"items": []}"#).unwrap(), []);
    }
}
