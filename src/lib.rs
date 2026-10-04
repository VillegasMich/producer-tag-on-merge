//! producer-tag-on-merge: plays your producer tag every time one of your pull requests
//! (GitHub) or merge requests (GitLab) is merged.
//!
//! Behavior is specified in `docs/architecture.md`; configuration in `docs/configuration.md`.

pub mod app;
pub mod clock;
pub mod config;
pub mod envfile;
pub mod exec;
pub mod hours;
pub mod http;
pub mod player;
pub mod poller;
pub mod preflight;
pub mod retry;
pub mod source;
pub mod state;
pub mod tags;
