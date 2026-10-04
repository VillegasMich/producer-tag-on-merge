# Repository setup

One-time settings for the GitHub repository so CI, the Docker image and releases work. Nothing
here is needed to *run* the tool on your own machine ([deployment.md](deployment.md) covers that).

| What                          | Where                                   | Needed for                          |
| ----------------------------- | --------------------------------------- | ----------------------------------- |
| `DOCKERHUB_USERNAME` variable | Settings → Secrets and variables → Actions → *Variables* | Publishing the image on release |
| `DOCKERHUB_TOKEN` secret      | Settings → Secrets and variables → Actions → *Secrets*   | Publishing the image on release |
| `DOCKERHUB_IMAGE` variable    | same, *Variables* (optional)            | Image name other than `<user>/producer-tag-on-merge` |
| `E2E_GITHUB_TOKEN` secret     | same, *Secrets* (optional)              | Live GitHub API test in CI          |
| `E2E_GITLAB_TOKEN` secret     | same, *Secrets* (optional)              | Live GitLab API test in CI          |
| `E2E_GITLAB_URL` variable     | same, *Variables* (optional)            | Live test against self-hosted GitLab |
| Workflow permissions          | Settings → Actions → General            | Release workflow pushing the version bump |
| Branch ruleset on `main`      | Settings → Rules → Rulesets             | Requiring CI before merging         |

Without any of them CI still runs: fmt, clippy, tests on Linux and macOS, the Docker build and its
smoke test. The live API tests skip themselves and nothing is pushed.

## What CI does

[`ci.yml`](../.github/workflows/ci.yml):

| Job      | Runs on                         | Does                                                                     |
| -------- | ------------------------------- | ------------------------------------------------------------------------ |
| `test`   | every push / PR, Linux + macOS  | `cargo fmt --check`, `clippy -D warnings`, `cargo test --locked`, `shellcheck scripts/*.sh` |
| `docker` | every push / PR                 | Builds the image, smoke test (`simulate --silent` with the sample tag), dry-run push. On a published release: multi-arch (`amd64`, `arm64`) push to Docker Hub. |
| `e2e`    | pushes to `main`, releases, manual runs (never PRs) | Read-only live API tests with the `E2E_*` tokens, if set.  |

[`release.yml`](../.github/workflows/release.yml): manual; bumps the version, creates the GitHub
release and triggers the image publish ([Releasing](#releasing)).

[`dependabot.yml`](../.github/dependabot.yml): weekly update PRs for crates, Actions and the
Docker base images.

## Docker Hub (publishing the image)

The `docker` job pushes when a GitHub release is **published**, or when CI is run manually on a
release tag with *publish* (what the release workflow does). Tag `v1.2.3` becomes image tags
`1.2.3`, `1.2` and `latest` (no `latest` for pre-releases like `v1.3.0-rc.1`).

1. Docker Hub → *Account settings* → *Personal access tokens* → *Generate new token*.
   Description `github-actions producer-tag-on-merge`, access **Read & Write**, an expiration you
   will remember. Copy it (shown once).
2. Optional: create the repository `producer-tag-on-merge` on Docker Hub first to choose its
   visibility; otherwise the first push creates it (public on free plans).
3. In the GitHub repository:

   ```bash
   gh secret set DOCKERHUB_TOKEN                       # paste the token at the prompt
   gh variable set DOCKERHUB_USERNAME --body <docker-hub-user>
   gh variable set DOCKERHUB_IMAGE --body <org>/producer-tag-on-merge   # optional
   ```

   Or in the web UI: *Settings* → *Secrets and variables* → *Actions*.

The job fails with a clear error if the secret or variable is missing when it needs them. Only
the publish path logs in, and secrets are never exposed to pull requests from forks.

The published image is the same one `scripts/install.sh` builds locally; to use it, replace
`producer-tag-on-merge` with `<docker-hub-user>/producer-tag-on-merge:<version>` in the
`docker run` command of [deployment.md](deployment.md#running-the-container-by-hand).

## Live API tests (optional)

`tests/e2e.rs` logs in and runs the real `WATCH=mine` queries over the last 30 days. Only `GET`
requests. In CI they run when the secrets exist:

- **`E2E_GITHUB_TOKEN`:** a fine-grained PAT (*Settings → Developer settings → Fine-grained
  tokens*) with **Public repositories (read-only)** access is enough (the search only needs to
  see some of your PRs). No write permissions. The built-in `GITHUB_TOKEN` won't work: it is an
  app token and `GET /user` rejects it. Secret names can't start with `GITHUB_`, hence `E2E_`.
- **`E2E_GITLAB_TOKEN`:** a GitLab PAT with only **`read_api`**. Set `E2E_GITLAB_URL` for a
  self-hosted instance.

```bash
gh secret set E2E_GITHUB_TOKEN
gh secret set E2E_GITLAB_TOKEN
gh variable set E2E_GITLAB_URL --body https://gitlab.example.com   # optional
```

Locally: `E2E_GITHUB=1 GITHUB_TOKEN=$(gh auth token) cargo test --test e2e github -- --nocapture`.

## Actions permissions

*Settings* → *Actions* → *General*:

- *Actions permissions*: allow GitHub Actions and reusable workflows (the workflows use
  `actions/*`, `docker/*`, `dtolnay/rust-toolchain`, `Swatinem/rust-cache`).
- *Workflow permissions*: **Read repository contents** is fine as the default; each workflow
  declares what it needs (`release.yml` asks for `contents: write` and `actions: write`).

## Protecting `main`

*Settings* → *Rules* → *Rulesets* → *New branch ruleset*, target `main`:

- Require a pull request before merging.
- Require status checks: `fmt, clippy, tests (ubuntu-latest)`, `fmt, clippy, tests
  (macos-latest)`, `Docker image (smoke test, push on release)`.
- Block force pushes.
- **Bypass list:** add the *GitHub Actions* app (or *Repository admin*), otherwise the release
  workflow can't push its version-bump commit to `main`.

## Releasing

### From GitHub Actions (recommended)

*Actions* → **Release** → *Run workflow* on `main`, or:

```bash
gh workflow run release.yml                       # bump from commit types (auto)
gh workflow run release.yml -f bump=minor
gh workflow run release.yml -f version=1.0.0-rc.1
gh workflow run release.yml -f dry_run=true       # only show what would happen
```

The workflow:

1. Checks that CI passed on the commit.
2. Computes the version with [`scripts/bump-version.sh`](../scripts/bump-version.sh): if
   `Cargo.toml`'s version is already released, bumps it from the Conventional Commits since the
   last tag (`feat`/`chore` → minor, breaking → major, else patch; breaking is minor while 0.x).
3. Commits the bump to `main`, creates tag + release with
   [`scripts/release.sh`](../scripts/release.sh) (generated notes).
4. Runs CI on the tag with *publish* and waits until the image is on Docker Hub.

It uses the built-in `GITHUB_TOKEN` (no extra secret) plus the Docker Hub settings above.

### Locally

From an up-to-date, clean `main` (`gh` logged in):

```bash
scripts/bump-version.sh minor && cargo build && git commit -am "chore(release): bump version to $(scripts/bump-version.sh)" && git push
scripts/release.sh --dry-run     # checks only
scripts/release.sh               # tag + GitHub release; CI then publishes the image
```

## Checklist

```bash
gh secret set DOCKERHUB_TOKEN
gh variable set DOCKERHUB_USERNAME --body <docker-hub-user>
gh secret set E2E_GITHUB_TOKEN        # optional
gh secret set E2E_GITLAB_TOKEN        # optional
gh secret list && gh variable list
```

Then push to `main` and check the *Actions* tab: `test` (both OSes), `docker` and `e2e` should be
green, and the `docker` job summary lists the tags a release would get.
