# CLAUDE.md

Guidance for Claude Code (and other AI assistants) working in this repository.

## What this project is

A Rust CLI that also runs as an always-on background service on a developer's computer (Linux and
macOS). It links a GitHub and/or GitLab account and, every time a pull request (GitHub) or merge
request (GitLab) is merged, plays the developer's **producer tag**: a short signature sound, like
the tag a music producer puts on their beats.

- Detection: polls the GitHub Search API / GitLab merge requests API (no webhooks — a laptop has
  no public endpoint).
- Playback: shells out to the system audio player (`paplay`, `pw-play`, `aplay`, `afplay`).
- Packaging: Docker image run from a systemd **user** unit on Linux; native binary as a launchd
  **LaunchAgent** on macOS (Docker Desktop can't reach CoreAudio; Docker on macOS is experimental).

Specs live in `README.md` and `docs/`. **Treat `docs/architecture.md` as the source of truth** for
behavior; update it in the same change when behavior changes. Sibling projects with the same shape
(CLI + daemon + Docker + service install): `../auto-git-commit-tool` and
`../claude-session-starter` — reuse their patterns (`exec.rs`, `clock.rs`, `retry.rs`, Dockerfile,
`scripts/install.sh`, systemd units).

## Commands

```bash
cargo build                    # build
cargo test                     # unit tests (no network, no real audio)
cargo clippy --all-targets -- -D warnings   # lint (must pass)
cargo fmt                      # format (must be clean)
docker build -t producer-tag-on-merge .
cargo run -- simulate          # fake merge through the real pipeline; plays your tag, no token
cargo run -- --env-file .env check          # config, tokens, tags, audio
cargo run -- --env-file .env once --dry-run # real API calls, plays nothing, state untouched
scripts/install.sh [docker|native]          # install as user service (no sudo)
```

## Hard rules

- **Read-only against GitHub/GitLab.** Only `GET` requests. Never create, comment, label,
  approve or merge anything. Docs must only ask for read scopes (`repo`/*Pull requests: Read*,
  `read_api`).
- **Each merge plays at most once.** Insert the event key into the persisted `seen` set *before*
  playing. Restarts, overlapping poll windows and API lag must never replay a tag.
- **Never replay history.** First start of a source marks everything seen and plays nothing.
  Merges older than `CATCH_UP_MINUTES` (sleep, offline, machine off) are skipped silently.
- **Tags never escape `TAGS_DIR`.** Author names come from the API; validate before building a
  path (no `/`, `\`, `..`, NUL; lower-case). Same for `tag set --for`.
- **Never log or print `GITHUB_TOKEN` / `GITLAB_TOKEN`**, API response bodies, or the env file
  contents. Never put a token on a command line or in a launchd plist. Wrap tokens in a type whose
  `Debug`/`Display` are redacted.
- **No shell.** Players and `PLAYER_COMMAND` are run via `std::process::Command` with an argv;
  never `sh -c`. Every child has a timeout (`PLAY_TIMEOUT_SECONDS`) and is killed after it.
- **User session only.** The service must run as the logged-in user (systemd `--user`,
  LaunchAgent). Never add a system-wide unit, LaunchDaemon, or anything requiring root: it would
  have no audio.
- **Linux and macOS both supported.** No Linux-only APIs in core logic; platform differences
  (player choice, default dirs, service install) live in `player.rs`, `config.rs` and the scripts.
  Default paths come from `dirs`, never hard-coded `/home/...`.
- **No audio crate, no forge SDK crate, no async runtime.** HTTP via `ureq` (rustls); sound via
  external players; `std::thread::sleep` in chunks ≤ 1 s, interruptible by signals.
- **Don't crash on transient failures** (network, 5xx, rate limits, audio server missing, player
  exiting non-zero). Log, back off, continue. Only config/preflight errors exit non-zero. One
  failing source must not stop the other.
- **Be gentle with rate limits.** Default poll 60 s, minimum 15 s. Use `ETag`/`If-None-Match`;
  honor `Retry-After` and `x-ratelimit-reset`. GitHub search allows 30 requests/min.
- Handle `SIGTERM`/`SIGINT` cleanly — in the container `tini` is PID 1 and forwards it. Let a
  running player finish, save state, exit 0.
- Times: store and compare in UTC (`chrono::Utc`). Only `QUIET_HOURS` is interpreted in
  `TIMEZONE`.
- State writes are atomic (temp file + rename). A corrupt state file = first start (nothing
  replays), never a crash.

## Conventions

- Rust edition 2024. Errors: `anyhow` at the top level, `thiserror` for module error types.
- Logging: `tracing`, level via `RUST_LOG`, to stdout (journald / launchd log file capture it).
- Config only from environment variables, optionally loaded with `--env-file` (real env wins).
  Add new settings to `docs/configuration.md`, the README table and `.env.example` together.
- Keep external interactions behind thin traits so the poller is unit tested with fakes:
  `MergeSource` (GitHub/GitLab), `Player`, `StateStore`, `Clock`. Command execution goes through
  `exec.rs`, HTTP through `http.rs`.
- Pure logic worth testing: dedupe/catch-up/quiet-hours/burst filters, tag lookup and path
  validation, `QUIET_HOURS` parsing, query building, response parsing (fixtures in
  `tests/fixtures/`, scrubbed of personal data), config and env-file parsing, player argv/volume.
- Service files live in `deploy/` (`deploy/systemd/*.service`, `deploy/launchd/agent.plist`) with
  placeholders the install script fills in.
- Crate layout: `src/lib.rs` holds the modules (so `tests/` can use them), `src/main.rs` is only
  the CLI. Integration tests: `tests/cli.rs` (offline binary runs), `tests/e2e.rs` (gated live).
- CI/release setup (secrets, Docker Hub, rulesets) is documented in `docs/repository-setup.md`;
  keep it in sync with `.github/workflows/`.
- Commit messages: Conventional Commits, validated against commitlint
  `@commitlint/config-conventional`.

## Commit message recommendation (required after every change)

At the end of **every** response that modifies files, recommend a commit message. Do not commit
unless explicitly asked — only suggest.

1. Inspect what is not yet staged/committed: `git status --short`, `git diff`, and untracked files
   (`git ls-files --others --exclude-standard`). Base the message on these changes only.
2. Follow commitlint `config-conventional`: header `type(scope?): subject`, max 100 characters,
   type one of `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`,
   `style`, `test`; imperative lower-case subject, no trailing period; optional body (why, lines
   ≤ 100 chars) and footer separated by blank lines. Breaking change: `type!:` and/or a
   `BREAKING CHANGE:` footer.
3. If changes are unrelated, suggest splitting them into several commits with the files for each.
4. Present it in a code block, ready to copy.

## Testing notes

- Unit tests never hit the network or play real audio. Use the trait fakes.
- `simulate` exercises poller + filters + tag lookup + real player without a token; use it first
  for anything touching playback. `simulate --silent` for CI/headless.
- `once --dry-run` is the safe way to test real API queries: it must not play or write state.
- Live API tests are gated behind `E2E_GITHUB=1` / `E2E_GITLAB=1` and are read-only.
- On Linux in Docker, audio only works with the host PulseAudio/PipeWire socket mounted and
  `--user $(id -u):$(id -g)`; if `play` is silent, check `pactl info` inside the container first.
