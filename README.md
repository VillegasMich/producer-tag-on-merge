# producer-tag-on-merge

Your PR just got merged. Your computer plays your **producer tag**.

Music producers stamp their beats with a short signature sound — a vocal drop, a sample, a
synth stab — so everyone knows who made it. This is the same idea for code: link your **GitHub**
and/or **GitLab** account, pick a short sound, and every time one of your pull requests (GitHub)
or merge requests (GitLab) is merged, it plays on your machine, wherever you are and whatever
you're doing.

It runs quietly in the background as a service on **Linux** and **macOS**, written in Rust and
shipped as a Docker image or a native binary. It only reads from GitHub/GitLab.

See [`docs/`](docs/) for the detailed design.

## How it works

1. **Link** – a read-only token for GitHub, GitLab (self-hosted works too), or both.
2. **Pick your tag** – any short `wav`/`ogg`/`mp3`… file becomes your signature
   ([how to make a good one](docs/tags.md)).
3. **Poll** – every 60 s the service asks the APIs for PRs/MRs merged since the last check
   (cheap conditional requests, no webhooks, no public endpoint needed).
4. **Play** – each new merge plays the author's tag once through the system audio
   (`paplay`/`pw-play` on Linux, `afplay` on macOS).
5. **Never replay** – the first start and anything older than the catch-up window (machine was
   asleep or off) are skipped silently. Each merge plays at most once, across restarts.

Two watch modes:

- **`mine`** (default) – your own PRs/MRs, in any repo.
- **`repos`** – every merge in a list of repos/projects; each teammate's own tag plays
  ([team tags](docs/tags.md#team-tags)).

## Quick start

### Linux (Docker + systemd user service)

Requires Docker (your user in the `docker` group, or rootless Docker) and PulseAudio or PipeWire
(`pipewire-pulse`), which current desktop distributions have by default.

```bash
scripts/install.sh            # asks for tokens and your tag, builds the image, starts the service
systemctl --user status producer-tag-on-merge
journalctl --user -u producer-tag-on-merge -f
```

### macOS (native LaunchAgent)

Requires a Rust toolchain. Docker Desktop can't reach the Mac's speakers, so the native binary is
the default here ([Docker on macOS is possible but experimental](docs/deployment.md#docker-on-macos-experimental)).

```bash
scripts/install.sh            # builds the binary, installs a LaunchAgent, starts it
tail -f ~/Library/Logs/producer-tag-on-merge.log
```

### Docker by hand (Linux)

```bash
docker build -t producer-tag-on-merge .
docker run -d --name producer-tag-on-merge --restart unless-stopped \
  --user "$(id -u):$(id -g)" \
  --env-file ~/.config/producer-tag-on-merge/env \
  -v "$XDG_RUNTIME_DIR/pulse/native:/run/pulse/native" \
  -v ~/.config/producer-tag-on-merge/tags:/tags:ro \
  -v ~/.local/share/producer-tag-on-merge:/data \
  producer-tag-on-merge
```

Details: [`docs/deployment.md`](docs/deployment.md).

## Command line

```text
producer-tag-on-merge [daemon]          # run forever: poll, play tags on new merges (default)
producer-tag-on-merge once [--dry-run]  # one poll cycle, then exit
producer-tag-on-merge status            # linked accounts, last poll, last tags played, next poll
producer-tag-on-merge check             # validate config, tokens, tags and audio
producer-tag-on-merge play [--author github:alice]   # play a tag now
producer-tag-on-merge tag set <file> [--for gitlab:jdoe] [--no-play]
producer-tag-on-merge tag list
producer-tag-on-merge simulate [--count N] [--author …] [--silent]   # fake merges, no network
```

Every command accepts `--env-file <path>` to load settings from a file.

## Configuration

Environment variables (or an env file, see [`.env.example`](.env.example)). At least one token is
required.

| Variable                | Default                  | Description                                              |
| ----------------------- | ------------------------ | -------------------------------------------------------- |
| `GITHUB_TOKEN`          | unset                    | GitHub token: classic `repo`, or fine-grained *Pull requests: Read*. |
| `GITHUB_API_URL`        | `https://api.github.com` | GitHub Enterprise API base.                              |
| `GITHUB_REPOS`          | unset                    | `WATCH=repos`: `owner/repo` or `owner`, comma-separated. |
| `GITLAB_TOKEN`          | unset                    | GitLab token with `read_api`.                            |
| `GITLAB_URL`            | `https://gitlab.com`     | Self-hosted GitLab.                                      |
| `GITLAB_PROJECTS`       | unset                    | `WATCH=repos`: project or group paths, comma-separated.  |
| `WATCH`                 | `mine`                   | `mine` (your PRs/MRs) or `repos` (everyone's, in the list). |
| `POLL_INTERVAL_SECONDS` | `60`                     | How often to check, 15–3600.                             |
| `CATCH_UP_MINUTES`      | `30`                     | After sleep/offline, merges older than this stay silent. |
| `MAX_PLAYS_PER_POLL`    | `3`                      | Burst cap when many PRs merge at once.                   |
| `TAGS_DIR`              | `<DATA_DIR>/tags`        | Tag files (`default.wav`, `github/<user>.wav`…).         |
| `PLAYER`                | `auto`                   | `auto`, `paplay`, `pw-play`, `aplay`, `afplay`, `command`, `none`. |
| `VOLUME`                | `100`                    | 0–100.                                                   |
| `QUIET_HOURS`           | unset                    | No sound in this range, e.g. `22:00-08:00`.              |
| `TIMEZONE`              | `UTC`                    | IANA zone for `QUIET_HOURS`, e.g. `America/Bogota`.      |
| `DATA_DIR`              | platform dependent       | State file location (`/data` in Docker).                 |
| `RUST_LOG`              | `info`                   | Log level.                                               |

Full details: [`docs/configuration.md`](docs/configuration.md).

## Documentation

- [`docs/architecture.md`](docs/architecture.md) – components, merge detection, dedupe, playback, failure handling
- [`docs/configuration.md`](docs/configuration.md) – every setting in detail
- [`docs/deployment.md`](docs/deployment.md) – Docker image, systemd user unit, macOS LaunchAgent, tokens
- [`docs/tags.md`](docs/tags.md) – making a good producer tag, file layout, team tags
- [`docs/testing.md`](docs/testing.md) – unit tests, `simulate`, manual end-to-end
- [`CLAUDE.md`](CLAUDE.md) – guidance for AI coding assistants working in this repo

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo run -- simulate          # hear your tag through the full pipeline, no token needed
```

## Status

Design stage: the documents above describe the planned behavior; the code is being written
against them.
