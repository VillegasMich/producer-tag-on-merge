//! Thin wrapper around [`std::process::Command`] used to run the audio player and `pactl`.
//!
//! Every command runs with an argv (never through a shell), with stdin closed, without the API
//! tokens in its environment, and with a hard timeout after which it is killed.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tracing::debug;

/// Default upper bound for any single external command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Never handed to child processes: a player has no business seeing them.
const SECRET_VARS: [&str; 2] = ["GITHUB_TOKEN", "GITLAB_TOKEN"];

const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("could not start `{program}` (is it installed and on PATH?): {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("i/o error while running `{command}`: {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` timed out after {}s and was killed", timeout.as_secs())]
    Timeout { command: String, timeout: Duration },
    #[error("`{command}` exited with {status}{}", stderr_suffix(stderr))]
    Failed {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
}

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// A command to run. Arguments must never contain secrets: they are visible in `ps` and logs.
#[derive(Debug, Clone)]
pub struct Cmd {
    program: OsString,
    args: Vec<OsString>,
    timeout: Duration,
}

impl Cmd {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Runs the command to completion (or until the timeout) and returns its stdout, trailing
    /// whitespace trimmed.
    pub fn run(&self) -> Result<String, ExecError> {
        let command = self.to_string();
        debug!(%command, "exec");

        let mut process = Command::new(&self.program);
        process
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in SECRET_VARS {
            process.env_remove(var);
        }

        let mut child = process.spawn().map_err(|source| ExecError::Spawn {
            program: self.program.to_string_lossy().into_owned(),
            source,
        })?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let status = match wait_timeout(&mut child, self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                // Reader threads are detached: grandchildren may still hold the pipes open.
                return Err(ExecError::Timeout {
                    command,
                    timeout: self.timeout,
                });
            }
            Err(source) => return Err(ExecError::Io { command, source }),
        };

        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if status.success() {
            Ok(stdout.trim_end().to_owned())
        } else {
            Err(ExecError::Failed {
                command,
                status,
                stderr: stderr.trim().to_owned(),
            })
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.program.to_string_lossy())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

/// Finds `program` like a shell would: as given if it contains a `/`, else in `PATH`.
pub fn find_program(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = Path::new(program);
        return is_executable(path).then(|| path.to_owned());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn wait_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout() {
        let out = Cmd::new("echo").arg("hello").run().unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn failure_carries_stderr() {
        let err = Cmd::new("ls")
            .arg("/definitely/not/a/real/path")
            .run()
            .unwrap_err();
        assert!(matches!(err, ExecError::Failed { ref stderr, .. } if !stderr.is_empty()));
    }

    #[test]
    fn missing_program_is_spawn_error() {
        let err = Cmd::new("definitely-not-a-real-program-xyz")
            .run()
            .unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }));
    }

    #[test]
    fn times_out_and_kills() {
        let started = Instant::now();
        let err = Cmd::new("sleep")
            .arg("5")
            .timeout(Duration::from_millis(100))
            .run()
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn display_joins_args() {
        let cmd = Cmd::new("paplay").args(["--volume=65536", "tag.wav"]);
        assert_eq!(cmd.to_string(), "paplay --volume=65536 tag.wav");
    }

    #[test]
    fn finds_programs_on_path() {
        assert!(find_program("sh").is_some());
        assert!(find_program("definitely-not-a-real-program-xyz").is_none());
        assert!(find_program("/definitely/not/here").is_none());
    }
}
