//! Minimal `KEY=value` parser for `--env-file`, in the format of `docker run --env-file`.
//!
//! One assignment per line, `#` comments and blank lines ignored, no quoting: everything after the
//! first `=` is the value. A line with only `KEY` (Docker's "take it from the host") is ignored,
//! since real environment variables win anyway. Values are never echoed in errors: the file holds
//! tokens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EnvFileError {
    #[error("cannot read env file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}:{line}: invalid variable name (expected KEY=value)", path.display())]
    Syntax { path: PathBuf, line: usize },
}

/// Settings from the real environment, falling back to the env file.
#[derive(Debug, Default, Clone)]
pub struct Env {
    file: HashMap<String, String>,
}

impl Env {
    /// Only the real environment.
    pub fn process() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, EnvFileError> {
        let content = std::fs::read_to_string(path).map_err(|source| EnvFileError::Read {
            path: path.to_owned(),
            source,
        })?;
        let file = parse(&content).map_err(|line| EnvFileError::Syntax {
            path: path.to_owned(),
            line,
        })?;
        Ok(Self { file })
    }

    /// A non-empty real environment variable wins over the file. An empty one (e.g. a blank
    /// `GITHUB_TOKEN=` passed through Docker) does not hide the file's value.
    pub fn get(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.file.get(key).cloned())
    }
}

/// Parses the file content. On error returns the 1-based line number.
pub fn parse(content: &str) -> Result<HashMap<String, String>, usize> {
    let mut vars = HashMap::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw).trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            if is_valid_key(line.trim_end()) {
                continue;
            }
            return Err(index + 1);
        };
        if !is_valid_key(key) {
            return Err(index + 1);
        }
        vars.insert(key.to_owned(), value.to_owned());
    }
    Ok(vars)
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn parses_docker_env_file_format() {
        let vars = parse(
            "# comment\n\nGITHUB_TOKEN=ghp_abc\n  WATCH=repos\r\nPLAYER_COMMAND=ffplay -nodisp {file}\nEMPTY=\nHOST_ONLY\n",
        )
        .unwrap();
        assert_eq!(vars["GITHUB_TOKEN"], "ghp_abc");
        assert_eq!(vars["WATCH"], "repos");
        assert_eq!(vars["PLAYER_COMMAND"], "ffplay -nodisp {file}");
        assert_eq!(vars["EMPTY"], "");
        assert!(!vars.contains_key("HOST_ONLY"));
    }

    #[test]
    fn value_keeps_equals_signs_and_quotes_verbatim() {
        let vars = parse("A=b=c\nB=\"quoted\"\n").unwrap();
        assert_eq!(vars["A"], "b=c");
        assert_eq!(vars["B"], "\"quoted\"");
    }

    #[test]
    fn reports_bad_line_number() {
        assert_eq!(parse("A=1\nnot valid=2\n"), Err(2));
        assert_eq!(parse("A=1\n\n=3\n"), Err(3));
        assert_eq!(parse("1A=x"), Err(1));
    }

    #[test]
    fn syntax_error_does_not_leak_the_value() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "bad key=ghp_secret").unwrap();
        let err = Env::load(file.path()).unwrap_err().to_string();
        assert!(!err.contains("ghp_secret"), "{err}");
        assert!(err.contains(":1:"), "{err}");
    }

    #[test]
    fn real_environment_wins_over_file() {
        // PATH is always set in the test environment; HOPEFULLY_UNSET_PTOM is not.
        let env = Env {
            file: HashMap::from([
                ("PATH".to_owned(), "from-file".to_owned()),
                ("HOPEFULLY_UNSET_PTOM".to_owned(), "from-file".to_owned()),
            ]),
        };
        assert_ne!(env.get("PATH").as_deref(), Some("from-file"));
        assert_eq!(
            env.get("HOPEFULLY_UNSET_PTOM").as_deref(),
            Some("from-file")
        );
        assert_eq!(env.get("ALSO_UNSET_PTOM"), None);
    }
}
