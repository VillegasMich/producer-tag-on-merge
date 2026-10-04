//! Live, read-only API tests. Skipped unless enabled, so `cargo test` never needs a token:
//!
//!   E2E_GITHUB=1 GITHUB_TOKEN=… cargo test --test e2e github
//!   E2E_GITLAB=1 GITLAB_TOKEN=… [GITLAB_URL=…] cargo test --test e2e gitlab
//!
//! They authenticate and run the real `WATCH=mine` queries over the last 30 days, asserting the
//! responses parse. Only `GET` requests: nothing is created or changed.

use chrono::{TimeDelta, Utc};
use producer_tag_on_merge::config::{Config, Watch};
use producer_tag_on_merge::source::{self, MergeSource, Platform};

fn enabled(flag: &str) -> bool {
    if std::env::var(flag).as_deref() == Ok("1") {
        return true;
    }
    eprintln!("{flag} is not set to 1; skipping live API test");
    false
}

fn config() -> Config {
    Config::from_lookup(|key| match key {
        "WATCH" => Some("mine".to_owned()),
        // Not used, but must be resolvable on CI runners without a home directory layout.
        "DATA_DIR" | "TAGS_DIR" => Some(std::env::temp_dir().display().to_string()),
        _ => std::env::var(key).ok(),
    })
    .expect("valid config")
}

fn check(source: &mut dyn MergeSource, platform: Platform) {
    let login = source.login().expect("token accepted");
    assert!(!login.is_empty());
    let since = Utc::now() - TimeDelta::days(30);
    let events = source.poll(since).expect("poll succeeds");
    eprintln!(
        "{}: {login}, {} merge(s) in 30 days",
        source.id(),
        events.len()
    );
    for e in &events {
        assert_eq!(e.platform, platform);
        assert!(e.key.starts_with(platform.as_str()));
        assert!(!e.repo.is_empty() && !e.author.is_empty());
        assert!(e.merged_at >= since);
    }
    // Same query again: served from the ETag cache (304) or identical; never an error.
    source.poll(since).expect("second poll succeeds");
}

#[test]
fn github_mine() {
    if !enabled("E2E_GITHUB") {
        return;
    }
    let config = config();
    assert_eq!(config.watch, Watch::Mine);
    let github = config.github.as_ref().expect("GITHUB_TOKEN is set");
    check(
        &mut source::github::GitHubSource::new(github, Watch::Mine),
        Platform::GitHub,
    );
}

#[test]
fn gitlab_mine() {
    if !enabled("E2E_GITLAB") {
        return;
    }
    let config = config();
    let gitlab = config.gitlab.as_ref().expect("GITLAB_TOKEN is set");
    check(
        &mut source::gitlab::GitLabSource::new(gitlab, Watch::Mine),
        Platform::GitLab,
    );
}
