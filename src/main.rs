//! Command-line entry point. See `README.md` and `docs/architecture.md`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use signal_hook::consts::{SIGINT, SIGTERM};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use producer_tag_on_merge::app::{self, SimulateOptions};
use producer_tag_on_merge::clock::SystemClock;
use producer_tag_on_merge::config::Config;
use producer_tag_on_merge::envfile::Env;

/// Your PR just got merged. Your computer plays your producer tag.
///
/// All configuration comes from environment variables (GITHUB_TOKEN, GITLAB_TOKEN, WATCH, ...),
/// optionally loaded from --env-file; see docs/configuration.md.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Load settings from this file (KEY=value per line, like `docker run --env-file`).
    /// Variables already set in the environment win.
    #[arg(long, global = true, value_name = "PATH")]
    env_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Run forever: poll, play tags on new merges. Default.
    Daemon,
    /// One poll cycle, then exit.
    Once {
        /// Real API calls, but play nothing and don't update the state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Linked accounts, last poll, last tags played, next poll. No network.
    Status,
    /// Validate config, tokens, tags and audio.
    Check,
    /// Play a tag now (default: yours). Tests the audio path end to end.
    Play {
        /// Whose tag: `github:alice`, `gitlab:jdoe` or `alice`.
        #[arg(long, value_name = "PLATFORM:USER")]
        author: Option<String>,
    },
    /// Manage tag files in TAGS_DIR.
    #[command(subcommand)]
    Tag(TagCommand),
    /// Push fake merges through the real pipeline (filters, tag lookup, player). No network,
    /// no token, the state file is not touched.
    Simulate(SimulateArgs),
}

#[derive(Debug, Clone, Subcommand)]
enum TagCommand {
    /// Copy an audio file into TAGS_DIR as your tag (or a teammate's), then preview it.
    Set {
        /// wav, ogg, flac, mp3, aiff or m4a, up to 5 MB.
        file: PathBuf,
        /// Whose tag: `github:alice`, `gitlab:jdoe` or `alice` (any platform). Default: yours.
        #[arg(long = "for", value_name = "PLATFORM:USER")]
        owner: Option<String>,
        /// Don't play the tag after installing it.
        #[arg(long)]
        no_play: bool,
    },
    /// List tag files and whose they are.
    List,
}

#[derive(Debug, Clone, Args)]
struct SimulateArgs {
    /// Number of merges at once (more than MAX_PLAYS_PER_POLL shows the burst cap).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=50))]
    count: u32,
    /// Author of the fake merges: `github:alice`, `gitlab:jdoe` or `alice`. Default: you.
    #[arg(long, value_name = "PLATFORM:USER")]
    author: Option<String>,
    /// Log instead of playing (PLAYER=none).
    #[arg(long)]
    silent: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Daemon);

    // Before logging is set up, since the env file may set RUST_LOG.
    let env = match cli.env_file.as_deref().map(Env::load).transpose() {
        Ok(env) => env.unwrap_or_else(Env::process),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    init_logging(&command, env.get("RUST_LOG"));

    let shutdown = match install_signal_handlers() {
        Ok(flag) => flag,
        Err(e) => {
            error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    match run(command, &env, Arc::clone(&shutdown)) {
        Ok(()) => ExitCode::SUCCESS,
        // Stopped while still starting up: not a failure.
        Err(e) if shutdown.load(Ordering::Relaxed) => {
            info!(reason = format!("{e:#}"), "stopped during startup");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, env: &Env, shutdown: Arc<AtomicBool>) -> Result<()> {
    let config = Config::from_lookup(|key| env.get(key))?;
    let clock = SystemClock::new(shutdown);
    match command {
        Command::Daemon => app::daemon(config, &clock),
        Command::Once { dry_run } => app::once(config, &clock, dry_run),
        Command::Status => app::status(config, &clock),
        Command::Check => app::check(config),
        Command::Play { author } => app::play(config, author.as_deref()),
        Command::Tag(TagCommand::Set {
            file,
            owner,
            no_play,
        }) => app::tag_set(config, &file, owner.as_deref(), !no_play),
        Command::Tag(TagCommand::List) => app::tag_list(config),
        Command::Simulate(args) => app::simulate(
            config,
            &clock,
            SimulateOptions {
                count: args.count as usize,
                author: args.author,
                silent: args.silent,
            },
        ),
    }
}

/// First SIGTERM/SIGINT requests a graceful shutdown (a running player finishes, state is
/// saved); a second one exits immediately.
fn install_signal_handlers() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [SIGTERM, SIGINT] {
        signal_hook::flag::register_conditional_shutdown(signal, 1, Arc::clone(&shutdown))
            .context("installing signal handler")?;
        signal_hook::flag::register(signal, Arc::clone(&shutdown))
            .context("installing signal handler")?;
    }
    Ok(shutdown)
}

fn init_logging(command: &Command, rust_log: Option<String>) {
    // Report-style commands print to stdout; keep logs quiet unless RUST_LOG says otherwise.
    let default_level = match command {
        Command::Status | Command::Check | Command::Tag(TagCommand::List) => "warn",
        _ => "info",
    };
    let filter = rust_log
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
}
