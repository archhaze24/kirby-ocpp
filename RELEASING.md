# Releasing kirby-ocpp

A release has three parts that must go out in this order: **GitHub release** (binaries, built by CI from a tag), **crates.io** (`cargo publish`), **npm** (wrapper packages assembled from the GitHub release assets, so `npx kirby-ocpp` works).

Do it from a machine where you are logged in to crates.io and npm. Replace `0.2.0` with the new version everywhere.

## 0. Prerequisites (once)

- `cargo login` with a crates.io API token (scopes `publish-new` + `publish-update`). The names `kirby-ocpp`, `kirby-ocpp-core`, `kirby-ocpp-tui` are published by the first release.
- `npm login` as `archhaze24` (2FA will prompt for an OTP on every `npm publish`). The package `kirby-ocpp` already exists on npm (TS 0.1.x); new versions must be higher than the latest, `0.1.2`.
- Node >= 18, `tar` (bsdtar on macOS is fine), `gh` (optional).

## 1. Prepare

1. `git switch main && git pull --ff-only` — CI on `main` must be green.
2. Bump the version in the root `Cargo.toml`: `[workspace.package] version` and the `version = "…"` of `kirby-ocpp-core` and `kirby-ocpp-tui` in `[workspace.dependencies]` (`0.2.0-dev` -> `0.2.0`). Run `cargo check --workspace` so `Cargo.lock` follows.
3. Update the "Install" section of `README.md` (these channels exist only after the release):
   ```
   npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001     # or: npm install -g kirby-ocpp
   cargo install kirby-ocpp
   # prebuilt binaries: https://github.com/archhaze24/kirby-ocpp/releases
   ```
4. `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
5. `cargo publish --dry-run -p kirby-ocpp-core` (must succeed; `kirby-ocpp-tui` and `kirby-ocpp` can only be dry-run after `kirby-ocpp-core` is on crates.io).
6. Commit (`release: 0.2.0`), push `main`, wait for CI.
7. Optional safety net: run the release workflow as a dry run (builds all five targets, publishes nothing): `gh workflow run release.yml` (default `dry_run=true`), then check it is green. The runners `macos-15-intel` and `ubuntu-24.04-arm` have not been exercised yet, so this is worth doing once.

## 2. GitHub release (binaries)

```
git tag -s v0.2.0 -m "v0.2.0"
git push origin v0.2.0
```
The `Release` workflow builds x86_64/aarch64 Linux, x86_64/aarch64 macOS and x86_64 Windows, then creates the GitHub release `v0.2.0` with auto-generated notes. Wait until it is green and check that the release has 10 files (5 archives + 5 `.sha256`). Edit the notes on GitHub if you want.

If a build fails, fix it, delete the tag (`git push origin :refs/tags/v0.2.0 && git tag -d v0.2.0`), and tag again. Nothing is published to crates.io/npm before this step succeeds.

## 3. crates.io

```
cargo publish -p kirby-ocpp-core
# wait ~1 minute until it appears in the index, then:
cargo publish -p kirby-ocpp-tui
cargo publish -p kirby-ocpp
```
`kirby-ocpp-testkit` is dev-only and is not published (`publish = false`). A published version cannot be overwritten; use `cargo yank --version 0.2.0 <crate>` if something is badly wrong.

## 4. npm

The wrapper has one launcher package (`kirby-ocpp`) and five platform packages under the owner's personal scope (`@archhaze24/kirby-ocpp-linux-x64`, `-linux-arm64`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`) that carry the binaries (unscoped names were rejected by npm's spam filter); npm installs only the one matching the machine.

```
node npm/build.mjs v0.2.0            # downloads the release assets, writes npm/dist/*
cd npm/dist
for p in kirby-ocpp-linux-x64 kirby-ocpp-linux-arm64 kirby-ocpp-darwin-x64 kirby-ocpp-darwin-arm64 kirby-ocpp-win32-x64; do
  (cd "$p" && npm publish --dry-run --access public)   # look at the file list, then repeat without --dry-run
done
(cd kirby-ocpp && npm publish --dry-run)   # main package LAST, otherwise installs briefly fail
```
Publish the five platform packages first with `npm publish --access public` (scoped packages need it), `kirby-ocpp` last (plain `npm publish`). The directory names in `npm/dist` have no scope (`kirby-ocpp-linux-x64` etc.). After 72 hours a version can no longer be unpublished; use `npm deprecate kirby-ocpp@0.2.0 "reason"` instead.

## 5. Verify

```
npx kirby-ocpp@0.2.0 --version
cargo install kirby-ocpp --version 0.2.0 --force && kirby-ocpp --version
```
Then bump `main` to the next development version (`0.3.0-dev`) when you start the next cycle.

## Notes

- `npm/build.mjs --assets <dir>` builds from already-downloaded archives instead of fetching them.
- The macOS binaries are unsigned/not notarized; a downloaded archive may need `xattr -d com.apple.quarantine kirby-ocpp`. Installing via npm/cargo does not trigger Gatekeeper.
- Musl (static Linux) builds are not part of the release yet.
