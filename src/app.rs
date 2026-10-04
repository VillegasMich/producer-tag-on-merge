//! The commands: wires config, sources, player, state and clock together.

use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use tracing::info;

use crate::clock::Clock;
use crate::config::Config;
use crate::player::{self, NullPlayer, Player};
use crate::poller::{CycleReport, Poller, Settings};
use crate::preflight::{self, Options};
use crate::source::fake::{ScriptedSource, event};
use crate::source::{self, Platform, gitlab};
use crate::state::{JsonFileStore, MemoryStore, SourceState, State, StateStore};
use crate::tags::{self, TagOwner};

/// Preflight, then poll until SIGTERM/SIGINT.
pub fn daemon(config: Config, clock: &dyn Clock) -> Result<()> {
    config.require_source()?;
    let mut sources = source::from_config(&config);
    let player = player::from_config(&config);
    let report = preflight::run(&config, &mut sources, &player, Options { audio: true });
    report.log();
    if report.has_errors() {
        bail!("preflight failed; fix the errors above (`producer-tag-on-merge check` lists them)");
    }
    let player = player?;

    let store = JsonFileStore::new(config.state_file());
    info!(
        watch = %config.watch,
        poll_interval_secs = config.poll_interval.as_secs(),
        state = %store.path().display(),
        tags_dir = %config.tags_dir.display(),
        "watching for merges"
    );
    let mut poller = Poller::new(
        sources,
        player.as_ref(),
        &store,
        clock,
        Settings::from_config(&config, false),
    );
    poller.run();
    Ok(())
}

/// One poll cycle. With `dry_run`: real API calls, nothing played, state untouched.
pub fn once(config: Config, clock: &dyn Clock, dry_run: bool) -> Result<()> {
    config.require_source()?;
    let mut sources = source::from_config(&config);
    let player: Result<Box<dyn Player>, _> = if dry_run {
        Ok(Box::new(NullPlayer))
    } else {
        player::from_config(&config)
    };
    let report = preflight::run(&config, &mut sources, &player, Options { audio: !dry_run });
    report.log();
    if report.has_errors() {
        bail!("preflight failed");
    }
    let player = player?;

    let store = JsonFileStore::new(config.state_file());
    let mut poller = Poller::new(
        sources,
        player.as_ref(),
        &store,
        clock,
        Settings::from_config(&config, dry_run),
    );
    let report = poller.cycle();
    log_summary(&report, dry_run);
    if report.failed > 0 {
        bail!(
            "{} source(s) could not be polled; see the log above",
            report.failed
        );
    }
    Ok(())
}

fn log_summary(report: &CycleReport, dry_run: bool) {
    info!(
        polled = report.polled,
        failed = report.failed,
        first_starts = report.first_starts,
        new = report.new,
        played = report.played.len(),
        skipped = report.skipped.len(),
        dry_run,
        "cycle done"
    );
}

/// Linked accounts, last polls and plays, from the config and the state file. No network.
pub fn status(config: Config, clock: &dyn Clock) -> Result<()> {
    let store = JsonFileStore::new(config.state_file());
    let state = store.load();
    let now = clock.now();

    let mut accounts: Vec<String> = Vec::new();
    if config.github.is_some() {
        accounts.push("github".to_owned());
    }
    if let Some(gitlab) = &config.gitlab {
        accounts.push(gitlab::source_id(gitlab.host()));
    }

    println!("Accounts:");
    if accounts.is_empty() {
        println!("  none linked (set GITHUB_TOKEN and/or GITLAB_TOKEN)");
    }
    for id in &accounts {
        match state.sources.get(id) {
            Some(s) => println!(
                "  {id:<20} {:<20} last successful poll {}",
                s.login.as_deref().unwrap_or("-"),
                ago(s.last_success, now)
            ),
            None => println!("  {id:<20} {:<20} never polled", "-"),
        }
    }
    println!("Watch:        {}", config.watch);
    let next = accounts
        .iter()
        .filter_map(|id| state.sources.get(id))
        .map(|s| s.last_success + TimeDelta::from_std(config.poll_interval).unwrap_or_default())
        .min();
    match next {
        Some(next) if next > now => println!(
            "Poll:         every {}s, next in ~{}s (if the service is running)",
            config.poll_interval.as_secs(),
            (next - now).num_seconds()
        ),
        _ => println!("Poll:         every {}s", config.poll_interval.as_secs()),
    }
    if let Some(q) = config.quiet_hours {
        println!("Quiet hours:  {q} ({})", config.timezone);
    }
    let default_tag = tags::find(&config.tags_dir, &TagOwner::Default);
    println!(
        "Tags:         {} (default tag: {})",
        config.tags_dir.display(),
        default_tag
            .map(|p| tags::display_name(&config.tags_dir, &p))
            .unwrap_or_else(|| "missing".to_owned())
    );
    println!("State:        {}", store.path().display());

    println!("Last played:");
    if state.last_played.is_empty() {
        println!("  nothing yet");
    }
    for p in &state.last_played {
        println!(
            "  {}  {}#{} by {} ({})",
            p.played_at.format("%Y-%m-%d %H:%M:%S UTC"),
            p.repo,
            p.number,
            p.author,
            p.tag
        );
    }
    Ok(())
}

fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds().max(0);
    let rel = match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    };
    format!("{} ({rel})", t.format("%Y-%m-%d %H:%M:%S UTC"))
}

/// Validates config, tokens, tags and audio; prints a report. Fails if anything is broken.
pub fn check(config: Config) -> Result<()> {
    let mut sources = source::from_config(&config);
    let player = player::from_config(&config);
    let report = preflight::run(&config, &mut sources, &player, Options { audio: true });
    println!(
        "Configuration: ok (WATCH={}, PLAYER={})",
        config.watch, config.player
    );
    for check in &report.checks {
        println!("{check}");
    }
    if report.has_errors() {
        bail!("some checks failed");
    }
    println!("All good.");
    Ok(())
}

/// Plays a tag now: the author's (`github:alice`, `alice`), or your default one.
pub fn play(config: Config, author: Option<&str>) -> Result<()> {
    let tag = match author {
        None => tags::find(&config.tags_dir, &TagOwner::Default),
        Some(spec) => match TagOwner::parse(spec)? {
            TagOwner::Author { platform, name } => tags::lookup(&config.tags_dir, platform, &name),
            TagOwner::Default => tags::find(&config.tags_dir, &TagOwner::Default),
        },
    };
    let Some(tag) = tag else {
        bail!(
            "no tag found in {} (not even default.*); add yours with `producer-tag-on-merge tag set <file>`",
            config.tags_dir.display()
        );
    };
    play_file(&config, &tag)
}

fn play_file(config: &Config, tag: &Path) -> Result<()> {
    let player = player::from_config(config)?;
    info!(tag = %tag.display(), "playing");
    player
        .play(tag)
        .with_context(|| format!("playing {}", tag.display()))
}

/// Installs `file` as a tag (default, or for `owner`), then previews it.
pub fn tag_set(config: Config, file: &Path, owner: Option<&str>, preview: bool) -> Result<()> {
    let owner = match owner {
        Some(spec) => TagOwner::parse(spec)?,
        None => TagOwner::Default,
    };
    let dest = tags::set(&config.tags_dir, file, &owner)?;
    println!("Installed {} as the tag for {owner}", dest.display());
    if preview {
        play_file(&config, &dest).context("the tag is installed, but the preview failed")?;
    }
    Ok(())
}

pub fn tag_list(config: Config) -> Result<()> {
    let dir = &config.tags_dir;
    let entries = if dir.is_dir() {
        tags::list(dir)?
    } else {
        Vec::new()
    };
    if entries.is_empty() {
        println!(
            "No tags in {}. Add yours with: producer-tag-on-merge tag set <file>",
            dir.display()
        );
        return Ok(());
    }
    println!("Tags in {}:", dir.display());
    for entry in entries {
        println!(
            "  {:<40} {}",
            tags::display_name(dir, &entry.path),
            entry.owner
        );
    }
    if tags::find(dir, &TagOwner::Default).is_none() {
        println!("No default tag: merges by authors without a tag play nothing.");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SimulateOptions {
    pub count: usize,
    /// `platform:user` or `user`; default: you, on GitHub.
    pub author: Option<String>,
    /// Log instead of playing.
    pub silent: bool,
}

/// Pushes fake merges through the real pipeline (dedupe, filters, tag lookup, player) with an
/// in-memory state. No network, no token, the real state file is not touched.
pub fn simulate(config: Config, clock: &dyn Clock, options: SimulateOptions) -> Result<()> {
    let (platform, author) = match options.author.as_deref() {
        None => (Platform::GitHub, "you".to_owned()),
        Some(spec) => match TagOwner::parse(spec)? {
            TagOwner::Author { platform, name } => (platform.unwrap_or(Platform::GitHub), name),
            TagOwner::Default => (Platform::GitHub, "you".to_owned()),
        },
    };
    let player: Box<dyn Player> = if options.silent {
        Box::new(NullPlayer)
    } else {
        player::from_config(&config)?
    };

    let now = clock.now();
    const SOURCE: &str = "simulate";
    let count = options.count as i64;
    let events = (0..count)
        .map(|i| {
            let merged_at = now - TimeDelta::seconds(count - i);
            event(&format!("{SOURCE}:{}", i + 1), platform, &author, merged_at)
        })
        .collect();
    // A source that already polled once, so this is not a first start.
    let mut state = State::default();
    state.sources.insert(
        SOURCE.to_owned(),
        SourceState {
            login: Some(author.clone()),
            last_success: now - TimeDelta::minutes(1),
        },
    );
    let store = MemoryStore::new(state);
    let source = ScriptedSource::new(SOURCE, platform).then(Ok(events));

    info!(count, %platform, author, silent = options.silent, "simulating merges");
    let mut poller = Poller::new(
        vec![Box::new(source)],
        player.as_ref(),
        &store,
        clock,
        Settings::from_config(&config, false),
    );
    let report = poller.cycle();
    log_summary(&report, false);
    if report.play_failures > 0 {
        bail!("{} play(s) failed; see the log above", report.play_failures);
    }
    Ok(())
}
