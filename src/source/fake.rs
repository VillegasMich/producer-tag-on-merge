//! Sources that don't touch the network: scripted results for tests and `simulate`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use chrono::{DateTime, Utc};

use super::{MergeEvent, MergeSource, Platform};
use crate::http::HttpError;

/// Returns queued poll results in order; an exhausted queue returns no events.
/// Records the `since` of every poll in a shared log.
pub struct ScriptedSource {
    id: String,
    platform: Platform,
    login: String,
    results: VecDeque<Result<Vec<MergeEvent>, HttpError>>,
    polls: Rc<RefCell<Vec<DateTime<Utc>>>>,
}

impl ScriptedSource {
    pub fn new(id: &str, platform: Platform) -> Self {
        Self {
            id: id.to_owned(),
            platform,
            login: "you".to_owned(),
            results: VecDeque::new(),
            polls: Rc::default(),
        }
    }

    pub fn then(mut self, result: Result<Vec<MergeEvent>, HttpError>) -> Self {
        self.results.push_back(result);
        self
    }

    /// Shared log of the `since` argument of each poll.
    pub fn polls(&self) -> Rc<RefCell<Vec<DateTime<Utc>>>> {
        Rc::clone(&self.polls)
    }
}

impl MergeSource for ScriptedSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn platform(&self) -> Platform {
        self.platform
    }

    fn login(&mut self) -> Result<String, HttpError> {
        Ok(self.login.clone())
    }

    fn cached_login(&self) -> Option<&str> {
        Some(&self.login)
    }

    fn check_targets(&mut self) -> Vec<(String, Result<(), HttpError>)> {
        Vec::new()
    }

    fn poll(&mut self, since: DateTime<Utc>) -> Result<Vec<MergeEvent>, HttpError> {
        self.polls.borrow_mut().push(since);
        self.results.pop_front().unwrap_or_else(|| Ok(Vec::new()))
    }
}

/// A merge event for tests and `simulate`.
pub fn event(key: &str, platform: Platform, author: &str, merged_at: DateTime<Utc>) -> MergeEvent {
    MergeEvent {
        key: key.to_owned(),
        platform,
        repo: "example/repo".to_owned(),
        number: key
            .rsplit(':')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1),
        title: "Simulated merge".to_owned(),
        author: author.to_owned(),
        merged_at,
        url: "https://example.invalid/merge".to_owned(),
    }
}
