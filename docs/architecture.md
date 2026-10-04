# Architecture

## Goal

Run unattended in the background of a developer's computer (Linux or macOS) and, every time a
pull request (GitHub) or merge request (GitLab) is merged, play that developer's **producer tag**:
a short signature sound, like the vocal or sound drop a music producer puts at the start of
their tracks ("…on the beat").

The service only **reads** from GitHub/GitLab and only **writes** sound to the local speakers.

## Concepts

| Term          | Meaning                                                                                  |
| ------------- | ---------------------------------------------------------------------------------------- |
| Source        | One linked account: GitHub (github.com or Enterprise) or GitLab (gitlab.com or self-hosted). Both can be linked at once. |
| Merge event   | A PR/MR that moved to merged. Identified by a stable key, e.g. `github:1234567890`.      |
| Producer tag  | An audio file (≤ 10 s recommended) played when a merge event fires.                      |
| Tags dir      | Folder holding tag files: your own default tag and, optionally, teammates' tags.         |
| Watch mode    | Which merges count: `mine` (PRs/MRs you authored) or `repos` (every merge in a list of repos/projects). |

## Design principles

- **Read-only on the forge.** Only `GET` requests. Never comment, label, approve or merge. Tokens
  need read scopes only.
- **Poll, don't listen.** A laptop has no public address, so webhooks are not used. The service
  polls the forge APIs (default every 60 s) with cheap, conditional requests.
- **Each merge plays at most once.** A persistent "seen" set makes restarts, overlaps and API lag
  harmless.
- **Never replay history.** First start marks everything as seen without playing. After the
  machine was asleep or off, only merges inside `CATCH_UP_MINUTES` play; older ones are skipped.
- **Single binary + system audio player.** The Rust binary decides; an existing player
  (`paplay`, `pw-play`, `aplay`, `afplay`) makes the sound, invoked via `std::process::Command`.
  This works the same natively and inside a container (through the PulseAudio socket) and avoids
  linking audio libraries.
- **Runs in the user session.** Sound needs the logged-in user's audio server, so the service is
  a systemd **user** unit (Linux) or a launchd **LaunchAgent** (macOS), never a system service.
- **Fail loud, keep running.** Network errors, rate limits and audio failures are logged and
  retried; only config/preflight errors exit non-zero.
- **Synchronous.** No async runtime; bounded `std::thread::sleep` chunks.
- **UTC internally.** `TIMEZONE` only affects `QUIET_HOURS`.

## Components

```
src/
├── main.rs          # CLI (clap), --env-file, logging, signal handlers, dispatch to app
├── lib.rs           # module tree (a library so tests/ can use it)
├── app.rs           # wires everything into the commands: daemon, once, status, check, play, tag, simulate
├── config.rs        # env var parsing + validation (Secret type for tokens)
├── envfile.rs       # minimal KEY=value parser for --env-file (same format as docker --env-file)
├── preflight.rs     # tokens valid (GET /user), tags dir + default tag, player available, audio server reachable
├── source/
│   ├── mod.rs       # trait `MergeSource`, struct `MergeEvent`
│   ├── github.rs    # GitHubSource: search API
│   ├── gitlab.rs    # GitLabSource: merge_requests API
│   └── fake.rs      # ScriptedSource for tests and `simulate`
├── http.rs          # ureq agent: auth headers, ETag cache, rate-limit / Retry-After handling
├── state.rs         # trait `StateStore`: JSON state file (cursors + seen set), in-memory store
├── tags.rs          # tag lookup per author, `tag set` / `tag list` validation
├── player.rs        # trait `Player`: CommandPlayer (paplay/pw-play/aplay/afplay/custom), NullPlayer
├── hours.rs         # QUIET_HOURS parsing and "is now inside" in TIMEZONE
├── poller.rs        # one poll cycle: fetch → dedupe → filter → play; and the daemon loop
├── retry.rs         # exponential backoff for failed polls
├── clock.rs         # `Clock` trait: SystemClock (sleep interruptible by SIGTERM), fake for tests
└── exec.rs          # Command wrapper: timeout, captured output, kill on timeout
```

```text
            ┌──────────── poller (every POLL_INTERVAL_SECONDS) ────────────┐
            │                                                              │
 GitHub ──► GitHubSource ─┐                                                │
                          ├─► MergeEvent[] ─► dedupe (state) ─► filters ─► tags ─► Player ─► speakers
 GitLab ──► GitLabSource ─┘                    seen set          catch-up    lookup    paplay / afplay
                                                                 quiet hours
```

## Command-line interface

Configuration always comes from environment variables (optionally loaded with
`--env-file <path>`); the CLI only selects what to do.

| Command                                   | Purpose                                                                 |
| ----------------------------------------- | ----------------------------------------------------------------------- |
| `daemon` (default, no args)               | Preflight, then poll forever.                                           |
| `once`                                    | Preflight, one poll cycle (plays new merges), exit.                     |
| `status`                                  | Read-only: linked accounts, watch mode, last poll per source, last merges played, next poll. |
| `check`                                   | Validate config, tokens, tags and audio, then exit.                     |
| `play [--author <platform>:<user>]`       | Play a tag now (default: your own). Tests the audio path end to end.    |
| `tag set <file> [--for <platform>:<user>]`| Validate a file and copy it into `TAGS_DIR` (default tag, or a teammate's), then preview it. `--no-play` skips the preview. |
| `tag list`                                | List tag files and which author each one belongs to.                    |
| `simulate [--count N] [--author …] [--silent]` | Push fake merge events through the real pipeline (dedupe, filters, tag lookup, player). No network, no token. |

Global flags: `--env-file <path>` (loaded before parsing config; real environment variables win),
`--dry-run` on `once` (fetch and log what would play, play nothing, don't update state).

## Preflight

Run at startup and by `check`:

1. At least one source configured (`GITHUB_TOKEN` and/or `GITLAB_TOKEN`).
2. For each source: `GET /user` succeeds → logs the login (needed for `WATCH=mine` and for your
   own tag). 401 → exit non-zero with a clear message. Network error → warning, the daemon still
   starts and retries.
3. `WATCH=repos`: the repo/project list is non-empty and each entry is readable (`GET` on it).
4. `TAGS_DIR` exists and holds a default tag (`default.{wav,ogg,flac,mp3,aiff,m4a}`), unless
   `PLAYER=none`.
5. Player: the selected binary exists. Audio server reachable: `pactl info` (Linux, PulseAudio or
   PipeWire-pulse) — warning only, since the user may not be logged in to a graphical session yet.

Token values and response bodies are never logged.

## Detecting merges

`MergeSource::poll(since) -> Result<Vec<MergeEvent>>`:

```rust
struct MergeEvent {
    key: String,              // "github:<id>" / "gitlab:<host>:<id>", unique and stable
    platform: Platform,       // GitHub | GitLab
    repo: String,             // "owner/repo" / "group/sub/project"
    number: u64,              // PR number / MR iid
    title: String,
    author: String,           // login / username
    merged_at: DateTime<Utc>,
    url: String,
}
```

`since = last_successful_poll - OVERLAP` (10 min), rounded down to the full hour. The overlap
covers search-index lag and clock skew; the rounding keeps the request URL identical between polls
so `ETag`s work (below). The seen set removes the duplicates both cause. On a first start
`last_successful_poll` is "now".

### GitHub

One Search API request per poll (`GET {GITHUB_API_URL}/search/issues`):

| Watch mode | Query (`q`)                                                                 |
| ---------- | --------------------------------------------------------------------------- |
| `mine`     | `is:pr is:merged merged:>=<since ISO 8601> author:<login>`                  |
| `repos`    | `is:pr is:merged merged:>=<since> repo:a/b repo:c/d user:acme` (entries OR'd; `user:` matches users and orgs) |

Parameters: `sort=updated&order=desc&per_page=50`. Headers: `Authorization: Bearer <token>`,
`Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, `User-Agent`.
`merged_at` comes from `item.pull_request.merged_at`; author from `item.user.login`.

Notes:

- Search is rate-limited to 30 requests/min per user; the default 60 s interval uses 1/min.
  On 403/429 honor `Retry-After` / `x-ratelimit-reset`.
- Search results can lag a minute or two behind the merge. That's the expected latency of the tag.
- Long `repos` lists can exceed the query length limit (256 chars): split into several queries
  (one request each per poll).
- Items that aren't merged PRs or miss fields are skipped, not fatal.
- If more than 50 results come back (huge catch-up), only the first page is used; the rest is
  older than the catch-up window anyway.

### GitLab

Header `PRIVATE-TOKEN: <token>`, base `{GITLAB_URL}/api/v4`.

| Watch mode | Request                                                                                      |
| ---------- | -------------------------------------------------------------------------------------------- |
| `mine`     | `GET /merge_requests?scope=created_by_me&state=merged&updated_after=<since>&order_by=updated_at&sort=desc&per_page=50` |
| `repos`    | `GET /projects/<url-encoded path>/merge_requests?state=merged&updated_after=<since>&…` per project, or `GET /groups/<path>/merge_requests?…` for a group (includes subgroups). Each path is resolved once: `GET /projects/<path>`, else `GET /groups/<path>`; a path that is neither is skipped with a warning. |

`updated_after` is a superset; keep only MRs with `merged_at >= since` (fall back to `updated_at`
when `merged_at` is null on old instances). Author: `author.username`. Repo: `references.full`
(before the `!`), else the path in `web_url`.

### Conditional requests

`http.rs` stores the `ETag` of each URL (in memory) and sends `If-None-Match`. A `304` means
"nothing new" and, on GitHub, does not count against the rate limit.

### Errors

| Response                                   | Meaning                                         |
| ------------------------------------------ | ----------------------------------------------- |
| `401`, or `403` without rate-limit headers | Token rejected / missing scope                  |
| `403`/`429` with `Retry-After`             | Rate limited for that many seconds              |
| `403`/`429` with `x-ratelimit-remaining: 0` (GitHub) or `ratelimit-remaining: 0` (GitLab) | Rate limited until `…-reset` (epoch) |
| `429` without hints                        | Rate limited, 60 s                              |
| `404`                                      | Not found or not visible to the token           |
| other, network, timeout (30 s)             | Transient                                       |

## State and deduplication

`DATA_DIR/state.json`:

```json
{
  "version": 1,
  "sources": {
    "github": { "login": "VillegasMich", "last_success": "2026-10-04T14:02:11Z" },
    "gitlab:gitlab.com": { "login": "villegasmich", "last_success": "2026-10-04T14:02:12Z" }
  },
  "seen": {
    "github:2874651234": "2026-10-04T13:58:40Z",
    "gitlab:gitlab.com:331245": "2026-10-04T11:20:03Z"
  },
  "last_played": [
    { "key": "github:2874651234", "repo": "acme/api", "number": 412, "author": "VillegasMich",
      "tag": "default.wav", "played_at": "2026-10-04T14:00:05Z" }
  ]
}
```

- **First start of a source** (no entry in `sources`): fetch, add every result to `seen`, play
  nothing, set `last_success = now`.
- **Normal poll:** for each event not in `seen`, add it to `seen`, then write the state *before*
  playing (a crash mid-play must not replay it), then apply the filters below. If the state
  can't be written, nothing plays that cycle (logged as "state not saved").
- `seen` entries (keyed by merge time) older than the catch-up window + `OVERLAP` + 1 day are
  pruned on every write; anything that old fails the catch-up filter anyway.
- `last_played` keeps the last 20 plays for `status`.
- Writes are atomic (write to `state.json.tmp`, `fsync`, rename). A corrupt or missing file is
  treated as a first start (nothing replays).

## Filters before playing

In order; a skipped event stays in `seen` and is only logged:

1. **Catch-up:** `merged_at < now - max(CATCH_UP_MINUTES, POLL_INTERVAL_SECONDS + OVERLAP)` → skip
   (machine was asleep/off; the moment is gone). The second term keeps the normal poll window
   (search lag included) playable, so `CATCH_UP_MINUTES=0` means "no catch-up after sleep" rather
   than "nothing ever plays".
2. **Quiet hours:** now inside `QUIET_HOURS` → skip.
3. **Burst cap:** at most `MAX_PLAYS_PER_POLL` plays per cycle, oldest merge first, with a 1 s gap.
   Extra events are logged as "skipped (burst)".
4. **Tag:** no file for the author and no `default.*` → skipped with a warning.

## Producer tags

### Lookup

For an event by `<author>` on `<platform>`, the first existing file wins
(extensions tried: `wav`, `ogg`, `flac`, `mp3`, `aiff`, `m4a`):

1. `TAGS_DIR/<platform>/<author>.<ext>` — a teammate's (or your own) tag for that platform
2. `TAGS_DIR/<author>.<ext>` — same author on any platform
3. `TAGS_DIR/default.<ext>` — your tag; also used for everyone without a tag

`<platform>` is `github` or `gitlab`; `<author>` is lower-cased. Names are validated before they
become a path: only ASCII letters, digits, `-`, `_`, `.`, `[`, `]` (bots: `dependabot[bot]`), no
leading dot, so no separators or `..` (an author name from the API must never escape `TAGS_DIR`).
An author whose name fails validation gets the default tag.

### `tag set`

Accepts `wav`, `ogg`, `flac`, `mp3`, `aiff`, `m4a` up to 5 MB, copies it to the lookup path
(replacing any file for the same author with another extension), and plays it once as a preview.
`--for` takes `github:<user>`, `gitlab:<user>` or `<user>` (any platform).
How to make a good tag: [tags.md](tags.md).

## Playing sound

`Player::play(file, volume) -> Result<()>`, run with `PLAY_TIMEOUT_SECONDS` (default 15; the
player is killed after it).

| `PLAYER`  | Command                                   | Notes                                             |
| --------- | ----------------------------------------- | ------------------------------------------------- |
| `auto`    | macOS: `afplay`. Linux: first of `paplay`, `pw-play`, `aplay` on `PATH`. | Default.                |
| `paplay`  | `paplay --volume=<0..65536> <file>`       | PulseAudio or PipeWire-pulse. Used in the Docker image. |
| `pw-play` | `pw-play --volume=<0.0..1.0> <file>`      | Native PipeWire.                                  |
| `aplay`   | `aplay -q <file>`                         | Raw ALSA, WAV only, no volume control. Last resort. |
| `afplay`  | `afplay -v <0.00..1.00> <file>`           | Built into macOS; `1` = unchanged level.          |
| `command` | `PLAYER_COMMAND` with `{file}` and `{volume}` (0–100) substituted, split into argv without a shell | Escape hatch. Must contain `{file}`. |
| `none`    | nothing, logs "would play …"              | Headless testing.                                 |

On failure (non-zero exit, timeout, audio server gone) the play is retried once after 5 s, then
dropped with a warning. A merge sound that arrives minutes late is worse than none.

Players run with stdin closed and without `GITHUB_TOKEN`/`GITLAB_TOKEN` in their environment.

## Daemon loop

1. Preflight.
2. Poll every source (independently; one failing source doesn't block the other).
3. Play what passed the filters.
4. Save state; sleep `POLL_INTERVAL_SECONDS`.

Each source has its own schedule. Sleeps are chunks of ≤ 1 s (signal latency) towards a
wall-clock deadline: the wall clock keeps running while the machine is suspended, so after a
resume the deadline has passed and the next poll happens immediately (the catch-up filter decides
what still plays). Failed polls back off 1, 2, 4, 8, 10 min (never sooner than the poll
interval); rate-limited ones wait until the reset; a rejected token pauses the source for 10 min.

**Shutdown:** first `SIGTERM`/`SIGINT` sets a flag checked by every sleep; a running player is
allowed to finish (≤ `PLAY_TIMEOUT_SECONDS`), state is saved, exit 0. A second signal exits
immediately.

## Failure handling

| Failure                                   | Behavior                                                        |
| ----------------------------------------- | --------------------------------------------------------------- |
| No source configured / invalid config     | Exit non-zero at startup.                                       |
| Token rejected (401) at startup           | Exit non-zero.                                                  |
| Token revoked/expired while running       | Log error, source paused, retried every 10 min; `check` reports it. Other source keeps running. |
| Network down / 5xx                        | Backoff retry; `last_success` not moved, so nothing is lost inside the catch-up window. |
| Rate limited (403/429)                    | Sleep until `Retry-After` / `x-ratelimit-reset`.                |
| No default tag                            | Exit non-zero (unless `PLAYER=none`).                           |
| Audio server not running (not logged in)  | Play fails → retried once, dropped, warning.                    |
| Player hangs                              | Killed after `PLAY_TIMEOUT_SECONDS`.                            |
| State file corrupt / missing              | Treated as first start (nothing replays), rewritten.            |

## Logging

Structured logs to stdout (`tracing`), level via `RUST_LOG`. At `info`: startup checks, new
merges, plays (repo, number, author, tag file) and skips with their reason, failures. At `debug`:
every request with its status (200/304/…) and per-poll counts, so an idle service stays quiet.
Tokens and API bodies are never logged. `status`, `check` and `tag list` default to `warn` since
they print a report.

## Platform support

| Platform | Recommended mode                     | Audio path                                                   |
| -------- | ------------------------------------ | ------------------------------------------------------------ |
| Linux    | Docker under a systemd user unit     | Host PulseAudio/PipeWire socket mounted into the container → `paplay` |
| Linux    | Native binary under a systemd user unit | `paplay` / `pw-play` directly                             |
| macOS    | Native binary as a LaunchAgent       | `afplay` (CoreAudio)                                         |
| macOS    | Docker (experimental)                | Docker Desktop's VM has no CoreAudio; needs a PulseAudio server on the host reached over TCP |

Details: [deployment.md](deployment.md).

## Suggested crates

- `ureq` (rustls) – HTTP
- `serde`, `serde_json` – API responses, state file
- `chrono`, `chrono-tz` – UTC timestamps, `QUIET_HOURS` in `TIMEZONE`
- `clap` – subcommands
- `anyhow` / `thiserror` – errors
- `tracing`, `tracing-subscriber` – logging
- `signal-hook` – SIGTERM/SIGINT (Linux and macOS)
- `dirs` – platform data/config directories for native defaults
- `percent-encoding` – GitLab project paths

No async runtime, no audio crate, no GitHub/GitLab SDK crate.

## Out of scope (for now)

- Webhooks / a public endpoint.
- Desktop notifications (could be added later as a second "output" next to the player).
- Windows.
- Recording or generating tags inside the tool (use any DAW, `ffmpeg`, `sox` or a TTS voice; see
  [tags.md](tags.md)).
