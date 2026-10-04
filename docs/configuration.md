# Configuration

All settings come from environment variables. They can also be loaded from a file with
`producer-tag-on-merge --env-file <path>` (format: `KEY=value`, one per line, `#` comments, no
quotes, no spaces around `=`; the same file works with `docker run --env-file`). Variables already
set in the environment win over the file. Invalid values abort startup with a clear error.

At least one of `GITHUB_TOKEN` / `GITLAB_TOKEN` is required.

## GitHub

### `GITHUB_TOKEN`

Token used to read pull requests. Unset = GitHub not linked. Never logged.

- **Classic PAT:** `repo` scope (needed to see private repos; `public_repo` is enough for public
  only).
- **Fine-grained PAT:** repository access to the repos you care about, **Pull requests: Read** and
  **Metadata: Read**. A fine-grained token is limited to one owner, so with `WATCH=mine` across
  several orgs use a classic token.
- Using your `gh` login: `GITHUB_TOKEN=$(gh auth token)`.

### `GITHUB_API_URL` (default: `https://api.github.com`)

For GitHub Enterprise Server: `https://<host>/api/v3`.

### `GITHUB_REPOS` (default: unset)

Only with `WATCH=repos`. Comma-separated list of `owner/repo` (one repo) or `owner` (every repo of
that user/org). Example: `acme/api,acme/web,VillegasMich`.

## GitLab

### `GITLAB_TOKEN`

Personal (or project/group) access token with the **`read_api`** scope. Unset = GitLab not linked.
Never logged.

### `GITLAB_URL` (default: `https://gitlab.com`)

Base URL of a self-hosted instance, e.g. `https://gitlab.example.com`.

### `GITLAB_PROJECTS` (default: unset)

Only with `WATCH=repos`. Comma-separated list of full paths: `group/project` (one project) or
`group` / `group/subgroup` (every project below it). Example: `acme/backend/api,acme/frontend`.

## What triggers a tag

### `WATCH` (default: `mine`)

| Value   | Plays a tag when…                                                              |
| ------- | ------------------------------------------------------------------------------ |
| `mine`  | a PR/MR **you authored** is merged, in any repo the token can see.             |
| `repos` | **any** PR/MR is merged in `GITHUB_REPOS` / `GITLAB_PROJECTS`. Each author's own tag plays if the tags dir has one (see [tags.md](tags.md#team-tags)). |

### `POLL_INTERVAL_SECONDS` (default: `60`)

How often to ask the APIs for new merges. `15`–`3600`. Lower means faster tags but more requests
(GitHub search allows 30/min).

### `CATCH_UP_MINUTES` (default: `30`)

After the machine was asleep, off, or offline, merges older than this are skipped silently.
`0` = only merges found in the normal poll window play. On the very first start nothing plays.

### `MAX_PLAYS_PER_POLL` (default: `3`)

Upper bound of tags played in one cycle (e.g. a release that merges 20 PRs at once). `1`–`20`.

## Sound

### `TAGS_DIR` (default: `<DATA_DIR>/tags`; Docker image: `/tags`)

Folder with the tag files. Must contain `default.<ext>` (your tag). Layout and lookup order:
[tags.md](tags.md#where-tags-live).

### `PLAYER` (default: `auto`)

`auto`, `paplay`, `pw-play`, `aplay`, `afplay`, `command` or `none`. `auto` picks `afplay` on
macOS and the first of `paplay`, `pw-play`, `aplay` on Linux. `none` only logs (headless tests).
See [architecture.md](architecture.md#playing-sound).

### `PLAYER_COMMAND` (default: unset)

Only with `PLAYER=command`. Program and arguments with `{file}` and `{volume}` (0–100)
placeholders; split on whitespace, no shell. Example: `ffplay -nodisp -autoexit -loglevel quiet {file}`.

### `VOLUME` (default: `100`)

`0`–`100`, mapped to the player's volume flag. Ignored by `aplay`.

### `PLAY_TIMEOUT_SECONDS` (default: `15`)

The player is killed after this. Keeps a very long file from blocking the loop.

### `QUIET_HOURS` (default: unset)

`HH:MM-HH:MM` in `TIMEZONE` during which merges are recorded but not played. Crossing midnight is
allowed: `22:00-08:00`.

### `TIMEZONE` (default: `UTC`)

IANA name used only for `QUIET_HOURS`, e.g. `America/Bogota`. The container's `TZ` is ignored.

## Runtime

### `DATA_DIR`

Where `state.json` lives (and `tags/` unless `TAGS_DIR` is set).

| Mode            | Default                                                          |
| --------------- | ---------------------------------------------------------------- |
| Docker image    | `/data` (mount a volume)                                         |
| Native, Linux   | `$XDG_DATA_HOME/producer-tag-on-merge` (`~/.local/share/…`)      |
| Native, macOS   | `~/Library/Application Support/producer-tag-on-merge`            |

### `RUST_LOG` (default: `info`)

`error`, `warn`, `info`, `debug`, `trace`, or a `tracing` filter like
`producer_tag_on_merge=debug`.

## Reference

| Variable                | Default                     | Description                                         |
| ----------------------- | --------------------------- | --------------------------------------------------- |
| `GITHUB_TOKEN`          | unset                       | GitHub read token. One of the two tokens required.  |
| `GITHUB_API_URL`        | `https://api.github.com`    | GitHub Enterprise API base.                         |
| `GITHUB_REPOS`          | unset                       | `WATCH=repos`: `owner/repo` or `owner`, comma list. |
| `GITLAB_TOKEN`          | unset                       | GitLab `read_api` token.                            |
| `GITLAB_URL`            | `https://gitlab.com`        | Self-hosted GitLab base URL.                        |
| `GITLAB_PROJECTS`       | unset                       | `WATCH=repos`: project or group paths, comma list.  |
| `WATCH`                 | `mine`                      | `mine` or `repos`.                                  |
| `POLL_INTERVAL_SECONDS` | `60`                        | Poll period, 15–3600.                               |
| `CATCH_UP_MINUTES`      | `30`                        | Max age of a merge that still plays after sleep.    |
| `MAX_PLAYS_PER_POLL`    | `3`                         | Burst cap per cycle.                                |
| `TAGS_DIR`              | `<DATA_DIR>/tags`           | Tag files.                                          |
| `PLAYER`                | `auto`                      | Audio player backend.                               |
| `PLAYER_COMMAND`        | unset                       | Custom player for `PLAYER=command`.                 |
| `VOLUME`                | `100`                       | 0–100.                                              |
| `PLAY_TIMEOUT_SECONDS`  | `15`                        | Kill the player after this.                         |
| `QUIET_HOURS`           | unset                       | No sound in this range.                             |
| `TIMEZONE`              | `UTC`                       | Zone for `QUIET_HOURS`.                             |
| `DATA_DIR`              | platform dependent          | State directory.                                    |
| `RUST_LOG`              | `info`                      | Log level.                                          |
