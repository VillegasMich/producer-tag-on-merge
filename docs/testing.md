# Testing

## Unit tests

```bash
cargo test
```

No network, no real audio. External interactions sit behind traits with fakes:

| Trait         | Real                                  | Fake in tests                                 |
| ------------- | ------------------------------------- | --------------------------------------------- |
| `MergeSource` | `GitHubSource`, `GitLabSource`        | returns scripted `MergeEvent` lists / errors  |
| `Player`      | `CommandPlayer`                       | records `(file, volume)` calls, can fail      |
| `StateStore`  | JSON file in `DATA_DIR`               | in-memory                                     |
| `Clock`       | `SystemClock`                         | manual time, sleeps advance it instantly      |

What must be covered:

- **Dedupe:** the same event in two polls plays once; overlap windows don't replay.
- **First start:** existing merges are marked seen, nothing plays.
- **Catch-up:** after a simulated sleep, merges inside `CATCH_UP_MINUTES` play, older ones don't.
- **Quiet hours:** parsing (`22:00-08:00` crosses midnight), matching in `TIMEZONE`, events marked
  seen but not played.
- **Burst cap:** `MAX_PLAYS_PER_POLL`, oldest first.
- **Tag lookup:** platform file > any-platform file > default; lower-casing; author names with
  `/`, `..` or other unsafe characters never resolve outside `TAGS_DIR`.
- **Parsing:** GitHub search and GitLab MR responses from fixtures in `tests/fixtures/`
  (real responses with personal data removed), including `merged_at: null` and missing fields.
- **HTTP behavior:** 304 handling, `Retry-After` / `x-ratelimit-reset` → next sleep, 401 →
  source paused.
- **Config:** validation errors, `--env-file` parsing, env var precedence over the file.
- **State:** atomic write, corrupt file → first start, pruning of old `seen` entries.
- **Player:** argv built per backend and volume mapping; `PLAYER_COMMAND` placeholder
  substitution without a shell.
- **Shutdown:** signal during sleep exits within 1 s; signal during play waits for the player.

## `simulate`

Runs the real poller, filters, tag lookup and player with a fake source. No token, no network.

```bash
cargo run -- simulate                              # one fake merge by you → your tag plays
cargo run -- simulate --count 5                    # burst: hear MAX_PLAYS_PER_POLL in action
cargo run -- simulate --author github:alice        # team tag lookup
cargo run -- simulate --silent                     # PLAYER=none, logs only
```

In Docker (Linux), with the same mounts as the service:

```bash
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$XDG_RUNTIME_DIR/pulse/native:/run/pulse/native" \
  -v ~/.config/producer-tag-on-merge/tags:/tags:ro \
  producer-tag-on-merge simulate
```

## Manual end-to-end

1. `check` – tokens, tags, player, audio server.
2. `play` – you hear your tag.
3. `once --dry-run` – real API calls; logs what would play, plays nothing, state untouched.
4. Merge a throwaway PR/MR in a scratch repo, wait one poll interval, hear the tag. `status`
   shows it under "last played".

## Optional live API tests

Gated so CI and normal `cargo test` never need tokens:

```bash
E2E_GITHUB=1 GITHUB_TOKEN=… cargo test --test e2e github
E2E_GITLAB=1 GITLAB_TOKEN=… cargo test --test e2e gitlab
```

Read-only: they run the real queries for `WATCH=mine` against the account and assert the
responses parse. They never merge or create anything.

## Lint and format

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI (planned, GitHub Actions): fmt, clippy, tests on `ubuntu-latest` and `macos-latest`, Docker
build on Linux.
