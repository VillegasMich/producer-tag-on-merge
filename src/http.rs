//! Read-only HTTP client for the forge APIs: `GET` only, `ETag` cache, rate-limit handling.
//!
//! Response bodies and request headers (which carry the token) are never logged.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use tracing::debug;

const TIMEOUT: Duration = Duration::from_secs(30);
/// Wait when a rate-limit response carries no usable hint.
const DEFAULT_RATE_LIMIT_WAIT: Duration = Duration::from_secs(60);
/// Upper bound on remembered `ETag`s; queries change over time, old ones are useless.
const MAX_ETAGS: usize = 64;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum HttpError {
    #[error("token rejected (HTTP {status}); check that it is valid and has the read scope")]
    Unauthorized { status: u16 },
    #[error("rate limited; next attempt in {}s", wait.as_secs())]
    RateLimited { wait: Duration },
    #[error("not found (HTTP 404), or the token can't see it")]
    NotFound,
    #[error("HTTP {status}")]
    Status { status: u16 },
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response: {0}")]
    Parse(String),
}

/// Result of a `GET`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    Body(String),
    /// `304 Not Modified`: same content as the last time this URL was fetched.
    NotModified,
}

pub struct HttpClient {
    agent: ureq::Agent,
    etags: HashMap<String, String>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .user_agent(concat!(
                "producer-tag-on-merge/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/VillegasMich/producer-tag-on-merge)"
            ))
            .build()
            .into();
        Self {
            agent,
            etags: HashMap::new(),
        }
    }

    /// `GET url` with the given headers. With `conditional`, sends the `ETag` from the previous
    /// response for the same URL and reports `304` as [`Fetched::NotModified`].
    pub fn get(
        &mut self,
        url: &str,
        headers: &[(&str, &str)],
        conditional: bool,
    ) -> Result<Fetched, HttpError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if conditional && let Some(etag) = self.etags.get(url) {
            request = request.header("If-None-Match", etag);
        }

        let mut response = request.call().map_err(|e| match e {
            // Never quote the URL or headers back: keep the message generic.
            ureq::Error::BadUri(_) => HttpError::Network("invalid URL".to_owned()),
            other => HttpError::Network(other.to_string()),
        })?;
        let status = response.status().as_u16();
        debug!(url, status, "GET");

        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        if status == 304 {
            return Ok(Fetched::NotModified);
        }
        if !(200..300).contains(&status) {
            return Err(classify_error(status, header, Utc::now().timestamp()));
        }
        let etag = header("etag");

        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| HttpError::Network(format!("reading response: {e}")))?;
        if conditional && let Some(etag) = etag {
            if self.etags.len() >= MAX_ETAGS && !self.etags.contains_key(url) {
                self.etags.clear();
            }
            self.etags.insert(url.to_owned(), etag);
        }
        Ok(Fetched::Body(body))
    }
}

/// Maps a non-2xx, non-304 response to an error. `header` looks up a response header,
/// `now_epoch` is the current Unix time (for `x-ratelimit-reset`).
pub fn classify_error(
    status: u16,
    header: impl Fn(&str) -> Option<String>,
    now_epoch: i64,
) -> HttpError {
    match status {
        401 => HttpError::Unauthorized { status },
        403 | 429 => {
            if let Some(wait) = rate_limit_wait(&header, now_epoch) {
                HttpError::RateLimited { wait }
            } else if status == 429 {
                HttpError::RateLimited {
                    wait: DEFAULT_RATE_LIMIT_WAIT,
                }
            } else {
                // A 403 without rate-limit headers: missing permission / scope.
                HttpError::Unauthorized { status }
            }
        }
        404 => HttpError::NotFound,
        _ => HttpError::Status { status },
    }
}

/// `Retry-After: <seconds>` (GitHub, GitLab), or `x-ratelimit-remaining: 0` with
/// `x-ratelimit-reset: <epoch>` (GitHub; GitLab sends `ratelimit-*`).
fn rate_limit_wait(header: &impl Fn(&str) -> Option<String>, now_epoch: i64) -> Option<Duration> {
    if let Some(secs) = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(Duration::from_secs(secs.max(1)));
    }
    for prefix in ["x-ratelimit", "ratelimit"] {
        let remaining = header(&format!("{prefix}-remaining"));
        let reset = header(&format!("{prefix}-reset")).and_then(|v| v.trim().parse::<i64>().ok());
        if remaining.as_deref().map(str::trim) == Some("0")
            && let Some(reset) = reset
        {
            let secs = (reset - now_epoch).clamp(1, 3600);
            return Some(Duration::from_secs(secs as u64));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn unauthorized() {
        assert_eq!(
            classify_error(401, headers(&[]), 0),
            HttpError::Unauthorized { status: 401 }
        );
        assert_eq!(
            classify_error(403, headers(&[]), 0),
            HttpError::Unauthorized { status: 403 }
        );
    }

    #[test]
    fn retry_after_wins() {
        let err = classify_error(
            429,
            headers(&[("Retry-After", "42"), ("x-ratelimit-remaining", "0")]),
            0,
        );
        assert_eq!(
            err,
            HttpError::RateLimited {
                wait: Duration::from_secs(42)
            }
        );
    }

    #[test]
    fn github_rate_limit_reset() {
        let err = classify_error(
            403,
            headers(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1000090"),
            ]),
            1_000_000,
        );
        assert_eq!(
            err,
            HttpError::RateLimited {
                wait: Duration::from_secs(90)
            }
        );
    }

    #[test]
    fn gitlab_rate_limit_reset() {
        let err = classify_error(
            429,
            headers(&[("RateLimit-Remaining", "0"), ("RateLimit-Reset", "1000010")]),
            1_000_000,
        );
        assert_eq!(
            err,
            HttpError::RateLimited {
                wait: Duration::from_secs(10)
            }
        );
    }

    #[test]
    fn reset_in_the_past_waits_at_least_a_second() {
        let err = classify_error(
            403,
            headers(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "5")]),
            1_000_000,
        );
        assert_eq!(
            err,
            HttpError::RateLimited {
                wait: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn bare_429_uses_default_wait() {
        assert_eq!(
            classify_error(429, headers(&[]), 0),
            HttpError::RateLimited {
                wait: DEFAULT_RATE_LIMIT_WAIT
            }
        );
    }

    #[test]
    fn other_statuses() {
        assert_eq!(classify_error(404, headers(&[]), 0), HttpError::NotFound);
        assert_eq!(
            classify_error(502, headers(&[]), 0),
            HttpError::Status { status: 502 }
        );
    }
}
