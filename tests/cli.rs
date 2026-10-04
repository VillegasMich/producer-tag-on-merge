//! Offline end-to-end tests of the binary: no network, no audio (PLAYER=none / --silent).

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_producer-tag-on-merge");
const SAMPLE_TAG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sample-tag.wav");

/// Runs the binary with a clean environment pointing at `dir`.
fn run(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("DATA_DIR", dir.join("data"))
        .env("TAGS_DIR", dir.join("tags"))
        .env("PLAYER", "none");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("running the binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn tag_set_list_and_simulate() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    let out = run(d, &["tag", "set", SAMPLE_TAG], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(d.join("tags/default.wav").is_file());

    let out = run(
        d,
        &[
            "tag",
            "set",
            SAMPLE_TAG,
            "--for",
            "gitlab:JDoe",
            "--no-play",
        ],
        &[],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(d.join("tags/gitlab/jdoe.wav").is_file());

    let out = run(d, &["tag", "list"], &[]);
    let listing = stdout(&out);
    assert!(
        listing.contains("default.wav") && listing.contains("gitlab:jdoe"),
        "{listing}"
    );

    let out = run(
        d,
        &["simulate", "--count", "4", "--author", "gitlab:jdoe"],
        &[],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert_eq!(log.matches("playing producer tag").count(), 3, "{log}");
    assert!(log.contains("gitlab/jdoe.wav"), "{log}");
    assert_eq!(
        log.matches("MAX_PLAYS_PER_POLL reached").count(),
        1,
        "{log}"
    );
    assert!(
        !d.join("data/state.json").exists(),
        "simulate never writes state"
    );
}

#[test]
fn tag_set_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &["tag", "set", SAMPLE_TAG, "--for", "github:../x"],
        &[],
    );
    assert!(!out.status.success());
    assert!(!dir.path().join("x.wav").exists());
}

#[test]
fn env_file_is_loaded_and_real_env_wins() {
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("env");
    std::fs::write(&env_file, "WATCH=repos\nPOLL_INTERVAL_SECONDS=120\n").unwrap();
    let env_path = env_file.to_str().unwrap();

    // WATCH=repos from the file; POLL_INTERVAL_SECONDS from the real environment.
    let out = run(
        dir.path(),
        &["--env-file", env_path, "status"],
        &[("POLL_INTERVAL_SECONDS", "90")],
    );
    let text = stdout(&out);
    assert!(out.status.success(), "{out:?}");
    assert!(text.contains("Watch:        repos"), "{text}");
    assert!(text.contains("every 90s"), "{text}");
}

#[test]
fn config_errors_exit_non_zero_without_leaking_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &["status"],
        &[
            ("GITHUB_TOKEN", "ghp_should_never_be_printed"),
            ("VOLUME", "loud"),
        ],
    );
    assert!(!out.status.success());
    let all = format!("{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
    assert!(all.contains("VOLUME"), "{all}");
    assert!(!all.contains("ghp_should_never_be_printed"), "{all}");

    let env_file = dir.path().join("env");
    std::fs::write(&env_file, "bad line with ghp_secret_value\n").unwrap();
    let out = run(
        dir.path(),
        &["--env-file", env_file.to_str().unwrap(), "status"],
        &[],
    );
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("ghp_secret_value"));
}

#[test]
fn daemon_needs_an_account() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["once"], &[]);
    assert!(!out.status.success());
    assert!(stdout(&out).contains("GITHUB_TOKEN"));
}
