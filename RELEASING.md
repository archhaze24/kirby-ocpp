# Releasing kirby-ocpp and kirby-csms

One release ships two binaries, the charge point emulator `kirby-ocpp` and the central system `kirby-csms`, with one shared version. It has three parts that must go out in this order: **GitHub release** (binaries, built by CI from a tag; the same workflow pushes the container image), **crates.io** (`cargo publish`), **npm** (wrapper packages assembled from the GitHub release assets, so `npx kirby-ocpp` and `npx kirby-csms` work). Afterwards push the Homebrew and Scoop files (step 6).

Do it from a machine where you are logged in to crates.io and npm. Replace `X.Y.Z` with the new version everywhere. For a prerelease use a SemVer prerelease version (e.g. `0.4.0-rc.1`, tag `v0.4.0-rc.1`).

## 0. Prerequisites (once)

- `cargo login` with a crates.io API token (scopes `publish-new` + `publish-update`). All six crate names (`kirby-ocpp`, `kirby-ocpp-core`, `kirby-ocpp-tui`, `kirby-csms`, `kirby-csms-core`, `kirby-csms-tui`) have been published since 0.4.0; a release only publishes new versions.
- `npm login` as `archhaze24` (2FA will prompt for an OTP on every `npm publish`). The unscoped launchers `kirby-ocpp` and `kirby-csms` and six `@archhaze24/kirby-ocpp-<platform>` packages already exist (0.4.0); the seventh platform package, `-linux-arm64-musl`, is new in 0.5 and is published by the first release that has it (publish it before the launchers).
- Node >= 18, `tar` (bsdtar on macOS is fine), `gh` (optional).

## 1. Prepare

1. Get the release commit onto `main`: from the development branch (e.g. `dev-0.5`) merge it into `main` (`git switch main && git pull --ff-only && git merge dev-0.5`; fast-forward or merge commit), push, and wait until CI on `main` is green.
2. Bump the version in the root `Cargo.toml`: `[workspace.package] version` and the `version = "…"` of all four internal crates in `[workspace.dependencies]`: `kirby-ocpp-core`, `kirby-ocpp-tui`, `kirby-csms-core`, `kirby-csms-tui` (drop the `-dev` suffix, e.g. `X.Y.Z-dev` -> `X.Y.Z`; `kirby-ocpp-testkit` has no version and stays unpublished). Run `cargo check --workspace` so `Cargo.lock` follows.
3. Check the "Install" section of `README.md`: it lists only channels that work after the release (npm, Cargo, release archives). Docker, Homebrew and Scoop stay in `docs/distribution.md` until the image, the tap and the bucket exist; add them to the README only after that.
4. Finish `CHANGELOG.md`: the section `## [X.Y.Z] - <date>` must exist with the release date and the notes (they become the GitHub release text; check the numbers in the statistics table). Keep an empty `## [Unreleased]` heading above it. At the bottom of the file add `[X.Y.Z]: .../compare/vPREV...vX.Y.Z` and point `[Unreleased]` at `vX.Y.Z...HEAD`. Preview with `.github/scripts/release-notes.sh vX.Y.Z`.
5. `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
6. `cargo publish --dry-run -p kirby-ocpp-core` (must succeed). The other crates can only be dry-run after the crates they depend on are on crates.io (`kirby-csms-core` needs `kirby-ocpp-core`; `kirby-ocpp-tui` needs `kirby-ocpp-core`; `kirby-csms-tui` needs `kirby-csms-core` and `kirby-ocpp-tui`; `kirby-ocpp` needs `-core` and `-tui`; `kirby-csms` needs all four libraries). `cargo package --list -p <crate>` works for all of them at any time: check that `THIRD_PARTY_NOTICES.md` is in `kirby-ocpp` and `kirby-csms`, and that `LICENSE` is in every published crate. The per-crate `LICENSE` files are plain copies of the root one and must stay identical: `for c in kirby-ocpp-core kirby-csms-core kirby-ocpp-tui kirby-csms-tui kirby-ocpp kirby-csms; do cmp LICENSE crates/$c/LICENSE; done`.
7. Commit (`release: X.Y.Z`), push `main`, wait for CI.
8. Optional safety net: run the release workflow manually (builds all seven targets with both binaries and the container image without pushing it; a manual run never creates a release): `gh workflow run release.yml --ref main`, then check it is green. The tag/version check is skipped on manual runs.

## 2. GitHub release (binaries)

```
git tag -s vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```
The `Release` workflow first checks that the tag (without the leading `v`) equals the workspace version in `Cargo.toml`, and fails before building otherwise (tag `v0.4.0` with `0.4.0-dev` in `Cargo.toml` is rejected). Then it builds x86_64 Linux (glibc and static musl), aarch64 Linux (glibc and static musl), x86_64/aarch64 macOS and x86_64 Windows, then creates the GitHub release `vX.Y.Z` with the notes from the `## [X.Y.Z]` section of `CHANGELOG.md` (`.github/scripts/release-notes.sh`; the run fails early if the section is missing or empty, and a prerelease tag without its own section falls back to `## [Unreleased]`); a tag containing `-` (e.g. `v0.4.0-rc.1`) is marked as a GitHub prerelease. Wait until it is green and check that the release has 14 files (7 archives + 7 `.sha256`) and that each archive contains both `kirby-ocpp[.exe]` and `kirby-csms[.exe]` (plus `LICENSE`, `README.md`, `THIRD_PARTY_NOTICES.md`, `THIRD_PARTY_NOTICES-kirby-csms.md` and `man/man1/*.1`, 15 pages; the `man` job generates them from the clap definitions, and `man -l man/man1/kirby-ocpp.1` opens one). Edit the notes on GitHub if you want.

The same run has two more jobs. `container image` builds the multi-arch image from the two musl archives and, on a tag push, pushes `ghcr.io/archhaze24/kirby-ocpp:X.Y.Z` (plus `X.Y` and `latest` only for a final version that is the highest final `v*` tag; see below). `GitHub release` also renders `kirby-ocpp.rb` (Homebrew) and `kirby-ocpp.json` (Scoop) into the workflow artifact `packaging` (not a release asset). The `container image` job needs `publish` and runs only when the release was created (or on a manual run, where it builds without pushing), so a failed release never leaves a pushed image. `X.Y` and `latest` are added only when the pushed tag equals the highest final tag in the repository (`git tag --list 'v*' | grep -v -e - | sort -V | tail -1`); re-tagging or patching an older line (say `v0.3.1` after `v0.4.0`) pushes just the exact `X.Y.Z` tag, so `latest` never moves backwards. **The first push creates a PRIVATE package**: once, in the GitHub package settings of `kirby-ocpp` make it public and link it to the repository (Package settings -> Change visibility; Connect repository). Check: `docker run --rm ghcr.io/archhaze24/kirby-ocpp:X.Y.Z --version` from a machine that is not logged in.

If a build fails, fix it, delete the tag (`git push origin :refs/tags/vX.Y.Z && git tag -d vX.Y.Z`), and tag again. Nothing is published to crates.io/npm before this step succeeds.

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
`kirby-ocpp-testkit` is dev-only and is not published (`publish = false`; no published crate lists it as a normal dependency, and its path-only dev-dependency entries are dropped by `cargo publish`). A published version cannot be overwritten; use `cargo yank --version X.Y.Z <crate>` if something is badly wrong.

## 4. npm

The wrapper has two launcher packages, `kirby-ocpp` and `kirby-csms`. Both use the same `npm/launcher.js`, which picks the binary by its own file name. It also has seven platform packages under the personal scope (`@archhaze24/kirby-ocpp-linux-x64`, `-linux-x64-musl`, `-linux-arm64`, `-linux-arm64-musl`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`) that carry both binaries in `bin/` (unscoped names were rejected by npm's spam filter); npm installs only the one matching the machine.

```
node npm/build.mjs vX.Y.Z            # downloads the release assets, writes npm/dist/*
cd npm/dist
for p in kirby-ocpp-linux-x64 kirby-ocpp-linux-x64-musl kirby-ocpp-linux-arm64 kirby-ocpp-linux-arm64-musl kirby-ocpp-darwin-x64 kirby-ocpp-darwin-arm64 kirby-ocpp-win32-x64; do
  (cd "$p" && npm publish --dry-run --access public)   # look at the file list, then repeat without --dry-run
done
(cd kirby-ocpp && npm publish --dry-run)   # launchers LAST, otherwise installs briefly fail
(cd kirby-csms && npm publish --dry-run)
```
Publish the seven platform packages first (a new platform package name must be published once before the launchers list it, which the order guarantees) with `npm publish --access public` (scoped packages need it), then the launchers `kirby-ocpp` (plain `npm publish`) and `kirby-csms` (plain `npm publish`, both are unscoped). For a prerelease version (contains `-`) add `--tag next` to every `npm publish`, otherwise it becomes `latest`.

`build.mjs` wipes its `--out` directory: it refuses the repo root, its ancestors, the home dir and `/`, and refuses a non-empty directory that lacks the `.kirby-npm-dist` marker it writes (delete an old `npm/dist` from before this check by hand once). The directory names in `npm/dist` have no scope (`kirby-ocpp-linux-x64` etc.). After 72 hours a version can no longer be unpublished; use `npm deprecate kirby-ocpp@X.Y.Z "reason"` instead.

## 5. Verify

```
npx kirby-ocpp@X.Y.Z --version
npx kirby-csms@X.Y.Z --version
cargo install kirby-ocpp --version X.Y.Z --force && kirby-ocpp --version
cargo install kirby-csms --version X.Y.Z --force && kirby-csms --version
```
Docker (from a logged-out machine, after the package is public): `docker run --rm ghcr.io/archhaze24/kirby-ocpp:X.Y.Z --version` and `docker run --rm ghcr.io/archhaze24/kirby-ocpp:X.Y.Z kirby-csms --version`.
Then bump `main` to the next development version (e.g. `0.6.0-dev`) when you start the next cycle.

## 6. Homebrew and Scoop

Download the workflow artifact `packaging` of the release run (`gh run download <run-id> -n packaging`; or render it yourself with `.github/scripts/render-packaging.sh vX.Y.Z`, which downloads the public `.sha256` files). Copy `kirby-ocpp.rb` to `Formula/kirby-ocpp.rb` in the repo `archhaze24/homebrew-tap` and `kirby-ocpp.json` to `bucket/kirby-ocpp.json` in `archhaze24/scoop-bucket` (create both repos once), commit and push. Check: `brew install archhaze24/tap/kirby-ocpp && kirby-csms --version`, `scoop bucket add archhaze24 https://github.com/archhaze24/scoop-bucket && scoop install kirby-ocpp`. Skip this for prereleases. Templates live in `packaging/`; the install channels are described in `docs/distribution.md`.

## Notes

- Lessons from 0.2.0: npm rejected new unscoped platform names (403 spam detection), so they live under the personal scope `@archhaze24/…` and need `--access public`; browser authentication may be requested for every `npm publish`; `npm unpublish` leaves `0.0.0-stage` placeholder packages behind (harmless). The first release is the exception: later releases reuse the existing scoped packages and need only `node npm/build.mjs vX.Y.Z` + the publish loop.

- `npm/build.mjs --assets <dir>` builds from already-downloaded archives instead of fetching them. Both modes verify every archive against its `.sha256` release asset (downloads fail when the checksum file is missing or differs; with `--assets` the check runs when the `.sha256` sits next to the archive, otherwise a warning is printed).
- The macOS binaries are unsigned/not notarized; a downloaded archive may need `xattr -d com.apple.quarantine kirby-ocpp kirby-csms`. Installing via npm/cargo does not trigger Gatekeeper.
- The `linux-x64-musl` package carries a fully static binary (`libc: ["musl"]`, so recent npm installs it only on Alpine-like systems; the launcher also detects musl at runtime and falls back to it when the glibc package is missing). The `linux-arm64-musl` package is the same for aarch64 (built natively on `ubuntu-24.04-arm`); the launcher prefers the musl package on musl systems of either architecture. A new platform package name must be published once before the main package lists it.
