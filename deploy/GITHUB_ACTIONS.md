# GitHub Actions: CI and container publishing

Workflow: `.github/workflows/ci.yml`, display name **CI**.

The Rust image is separate from the original Python image:

```text
ghcr.io/d3-3109/workbuddy2api-hub-rust
```

The repository path is derived dynamically from `github.repository` and lowercased. The workflow works in another fork without hardcoding credentials or registry ownership.

## Trigger and tags

- Push to `rust`: test, build, container-smoke both architectures; publish `edge` and `sha-<full commit>`. Documentation-only pushes (`docs/**`, `*.md`) are skipped.
- Pull request targeting `rust`: test/build only, no GHCR login or package write permission.
- Push `rust-v<workspace-version>`: publish the matching version tag and `sha-<full commit>` after verification. Version must match `Cargo.toml`.
- Manual dispatch: `publish` defaults to true, but publishing is permitted only from `rust` or a `rust-v*` tag.

This does **not** create a GitHub Release. The Python workflows on `main` react to *any* published release and to `v*` tags, so do not publish a GitHub Release for the Rust branch, and do not use plain `v*` tags for Rust.

`edge` is a mutable alpha branch tag; use the commit tag or registry digest for reproducible deployment. No `latest` production-stability claim is made.

## Pipeline gates

Each architecture runs natively (`ubuntu-24.04`, `ubuntu-24.04-arm`):

1. Install Rust1.99.1 and native C/Perl build tools.
2. Run formatting, clippy with `-D warnings`, the complete locked Cargo workspace tests, and build a local test executable.
3. Run both existing gateway and upstream-sync smoke suites using loopback fake upstreams.
4. Build the real Alpine/musl image with the Dockerfile-specific source allowlist.
5. Run `container_smoke.py` against that actual image: UID10001, read-only root, dropped capabilities, tmpfs data, CA bundle, authentication/panel API, dashboard, bounded clean shutdown.
6. Export the **tested image** with SHA256 into the architecture artifact.

Only when both jobs succeed does a separate publish job receive `packages:write`. It downloads/verifies the image archives, pushes those exact images (no untested rebuild), and assembles the AMD64+ARM64 manifest. Official Actions are pinned by commit SHA.

Source tests run from a full checkout because test fixtures are intentionally excluded from the Docker build context. Production image builds use an explicit musl target so static CRT flags do not break host proc-macro compilation.

## Authentication and package visibility

GHCR publication uses the repository-provided `GITHUB_TOKEN`, not an embedded PAT. No real WorkBuddy account credentials are needed for CI. Tests use temporary synthetic values.

For an existing fork, enable Actions if GitHub requires it. A package initially created as private may require explicit visibility changes or authenticated pulls; a public source repository does not by itself prove anonymous package access. Check the package page before expecting unauthenticated VPS pulls.

## Pull on the VPS

After a successful publish:

```sh
docker pull ghcr.io/d3-3109/workbuddy2api-hub-rust:edge
```

Or use the root `docker-compose.yml`, which runs the published image without a source checkout (see the README):

```sh
cp .env.example .env   # fill in API_KEY and WB_PANEL_PASSWORD
docker compose up -d
```

To build from source instead, use `deploy/compose.yml` (`docker compose --env-file deploy/.env -f deploy/compose.yml up -d --build`), or add `deploy/compose.registry.yml` with `--no-build` to run the published image with that file.

Set private `API_KEY` and nondefault `WB_PANEL_PASSWORD` first. The default published port remains `127.0.0.1:8788`; use an SSH tunnel or TLS reverse proxy. Named data volumes remain separate from the old Python deployment.

The image supports `linux/amd64` and `linux/arm64` only after the corresponding native Actions jobs have passed. Local Windows cross-build results do not replace these Linux runtime gates. Real upstream acceptance/performance are separate from these offline/container checks.
