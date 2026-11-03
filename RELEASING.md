# Releasing kirby-ocpp and kirby-csms

One release ships two binaries, the charge point emulator `kirby-ocpp` and the central system `kirby-csms`, with one shared version. It has three parts that must go out in this order: **GitHub release** (binaries, built by CI from a tag), **crates.io** (`cargo publish`), **npm** (wrapper packages assembled from the GitHub release assets, so `npx kirby-ocpp` and `npx @archhaze24/kirby-csms` work).

Do it from a machine where you are logged in to crates.io and npm. Replace `0.2.0` with the new version everywhere.

## 0. Prerequisites (once)

- `cargo login` with a crates.io API token (scopes `publish-new` + `publish-update`). The names `kirby-ocpp`, `kirby-ocpp-core`, `kirby-ocpp-tui` are already published; `kirby-csms`, `kirby-csms-core`, `kirby-csms-tui` are new and are published by the first release that contains them.
- `npm login` as `archhaze24` (2FA will prompt for an OTP on every `npm publish`). The package `kirby-ocpp` and the six `@archhaze24/kirby-ocpp-<platform>` packages already exist; `@archhaze24/kirby-csms` is new (first publish needs `--access public`).
- Node >= 18, `tar` (bsdtar on macOS is fine), `gh` (optional).

## 1. Prepare

1. `git switch main && git pull --ff-only` — CI on `main` must be green.
2. Bump the version in the root `Cargo.toml`: `[workspace.package] version` and the `version = "…"` of all four internal crates in `[workspace.dependencies]`: `kirby-ocpp-core`, `kirby-ocpp-tui`, `kirby-csms-core`, `kirby-csms-tui` (`0.2.0-dev` -> `0.2.0`; `kirby-ocpp-testkit` has no version and stays unpublished). Run `cargo check --workspace` so `Cargo.lock` follows.
3. Update the "Install" section of `README.md` (these channels exist only after the release):
   ```
   npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001     # or: npm install -g kirby-ocpp
   cargo install kirby-ocpp
   npx @archhaze24/kirby-csms --listen 0.0.0.0:9000           # or: npm install -g @archhaze24/kirby-csms
   cargo install kirby-csms
   # prebuilt binaries: https://github.com/archhaze24/kirby-ocpp/releases
   ```
4. `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
5. `cargo publish --dry-run -p kirby-ocpp-core` (must succeed). The other crates can only be dry-run after the crates they depend on are on crates.io (`kirby-csms-core` needs `kirby-ocpp-core`; `kirby-ocpp-tui` needs `kirby-ocpp-core`; `kirby-csms-tui` needs `kirby-csms-core` and `kirby-ocpp-tui`; `kirby-ocpp` needs `-core` and `-tui`; `kirby-csms` needs all four libraries). `cargo package --list -p <crate>` works for all of them at any time: check that `THIRD_PARTY_NOTICES.md` is in `kirby-ocpp` and `kirby-csms`.
6. Commit (`release: 0.2.0`), push `main`, wait for CI.
7. Optional safety net: run the release workflow as a dry run (builds all six targets with both binaries, publishes nothing): `gh workflow run release.yml` (default `dry_run=true`), then check it is green. The runners `macos-15-intel` and `ubuntu-24.04-arm` have not been exercised yet, so this is worth doing once.

## 2. GitHub release (binaries)

```
git tag -s v0.2.0 -m "v0.2.0"
git push origin v0.2.0
```
The `Release` workflow builds x86_64 Linux (glibc and static musl), aarch64 Linux, x86_64/aarch64 macOS and x86_64 Windows, then creates the GitHub release `v0.2.0` with auto-generated notes. Wait until it is green and check that the release has 12 files (6 archives + 6 `.sha256`) and that each archive contains both `kirby-ocpp[.exe]` and `kirby-csms[.exe]` (plus `LICENSE`, `README.md`, `THIRD_PARTY_NOTICES.md`, `THIRD_PARTY_NOTICES-kirby-csms.md`). Edit the notes on GitHub if you want.

If a build fails, fix it, delete the tag (`git push origin :refs/tags/v0.2.0 && git tag -d v0.2.0`), and tag again. Nothing is published to crates.io/npm before this step succeeds.

## 3. crates.io

```
cargo publish -p kirby-ocpp-core
# wait ~1 minute until it appears in the index, then:
cargo publish -p kirby-ocpp-tui
cargo publish -p kirby-csms-core
# wait for the index again, then:
cargo publish -p kirby-csms-tui
cargo publish -p kirby-ocpp
cargo publish -p kirby-csms
```
Order follows the dependencies: `kirby-ocpp-core` -> (`kirby-ocpp-tui`, `kirby-csms-core`) -> `kirby-csms-tui` (needs both) -> (`kirby-ocpp`, `kirby-csms`). Wait for the index before each crate whose dependency was just published.
`kirby-ocpp-testkit` is dev-only and is not published (`publish = false`; no published crate lists it as a normal dependency, and its path-only dev-dependency entries are dropped by `cargo publish`). A published version cannot be overwritten; use `cargo yank --version 0.2.0 <crate>` if something is badly wrong.

## 4. npm

The wrapper has two launcher packages (`kirby-ocpp` and `@archhaze24/kirby-csms`, both using the same `npm/launcher.js`, which picks the binary by its own file name) and six platform packages under the owner's personal scope (`@archhaze24/kirby-ocpp-linux-x64`, `-linux-x64-musl`, `-linux-arm64`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`) that carry both binaries in `bin/` (unscoped names were rejected by npm's spam filter); npm installs only the one matching the machine.

```
node npm/build.mjs v0.2.0            # downloads the release assets, writes npm/dist/*
cd npm/dist
for p in kirby-ocpp-linux-x64 kirby-ocpp-linux-x64-musl kirby-ocpp-linux-arm64 kirby-ocpp-darwin-x64 kirby-ocpp-darwin-arm64 kirby-ocpp-win32-x64; do
  (cd "$p" && npm publish --dry-run --access public)   # look at the file list, then repeat without --dry-run
done
(cd kirby-ocpp && npm publish --dry-run)   # launchers LAST, otherwise installs briefly fail
(cd kirby-csms && npm publish --dry-run --access public)
```
Publish the six platform packages first with `npm publish --access public` (scoped packages need it), then the launchers `kirby-ocpp` (plain `npm publish`) and `kirby-csms` (`npm publish --access public`, it is the scoped `@archhaze24/kirby-csms`). The directory names in `npm/dist` have no scope (`kirby-ocpp-linux-x64` etc.). After 72 hours a version can no longer be unpublished; use `npm deprecate kirby-ocpp@0.2.0 "reason"` instead.

## 5. Verify

```
npx kirby-ocpp@0.2.0 --version
npx @archhaze24/kirby-csms@0.2.0 --version
cargo install kirby-ocpp --version 0.2.0 --force && kirby-ocpp --version
cargo install kirby-csms --version 0.2.0 --force && kirby-csms --version
```
Then bump `main` to the next development version (e.g. `0.4.0-dev`) when you start the next cycle.

## Notes

- Lessons from 0.2.0: npm rejected new unscoped platform names (403 spam detection), so they live under the personal scope `@archhaze24/…` and need `--access public`; browser authentication may be requested for every `npm publish`; `npm unpublish` leaves `0.0.0-stage` placeholder packages behind (harmless). The first release is the exception: later releases reuse the existing scoped packages and need only `node npm/build.mjs vX.Y.Z` + the publish loop.

- `npm/build.mjs --assets <dir>` builds from already-downloaded archives instead of fetching them.
- The macOS binaries are unsigned/not notarized; a downloaded archive may need `xattr -d com.apple.quarantine kirby-ocpp kirby-csms`. Installing via npm/cargo does not trigger Gatekeeper.
- The `linux-x64-musl` package carries a fully static binary (`libc: ["musl"]`, so recent npm installs it only on Alpine-like systems; the launcher also detects musl at runtime and falls back to it when the glibc package is missing). An aarch64 musl build is not included; add `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm` the same way if needed. A new platform package name must be published once before the main package lists it.
