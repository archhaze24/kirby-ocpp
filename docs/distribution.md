# Distribution

Every way to get `kirby-ocpp` (charge point emulator) and `kirby-csms` (central system). All channels ship both binaries with one shared version.

Contents: [npm](#npm) · [Cargo](#cargo) · [Release archives](#release-archives) · [Docker](#docker) · [Homebrew](#homebrew) · [Scoop](#scoop) · [Build the image from source](#build-the-image-from-source) · [Related pages](#related-pages)

## npm

```bash
npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
npx kirby-csms --listen 0.0.0.0:9000
npm install -g kirby-ocpp kirby-csms
```

The launcher packages pick a platform package (`@archhaze24/kirby-ocpp-<os>-<arch>[-musl]`): Linux x64 and arm64 (glibc and static musl), macOS x64 and arm64, Windows x64. On musl systems (Alpine) the launcher prefers the musl package and falls back to it when the glibc one is missing.

## Cargo

```bash
cargo install kirby-ocpp
cargo install kirby-csms
```

Needs Rust 1.88 or newer.

## Release archives

[GitHub releases](https://github.com/archhaze24/kirby-ocpp/releases) carry one archive per target, `kirby-ocpp-<tag>-<target>.tar.gz` (`.zip` on Windows) plus a `.sha256` file each. Targets: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. The musl builds are fully static (Alpine, minimal containers). Each archive holds both binaries, the license, the third-party notices, the README and the manual pages in `man/man1/` (`kirby-ocpp.1`, `kirby-csms.1` and one page per `kirby-ocpp` subcommand, such as `kirby-ocpp-run.1`; the Windows zip carries them too). The pages are generated from the same definitions as `--help`. Read one in place with `man -l man/man1/kirby-ocpp.1`, or install them with `cp man/man1/*.1 /usr/local/share/man/man1/`.

The archives also include `kirby-csms-openapi.json`, the machine-readable control API
contract. The same file is available as a standalone release asset. Manual `Release` runs
upload it as the workflow artifact `openapi` without creating a release.

## Docker

Image: `ghcr.io/archhaze24/kirby-ocpp`, multi-arch (`linux/amd64`, `linux/arm64`), built from the static musl binaries on every version tag. Tags: `X.Y.Z`, plus `X.Y` and `latest` when the release is the highest final version so far (prereleases and re-tagged older versions only get `X.Y.Z`; `latest` never moves backwards). It runs as the non-root user `kirby` (uid 10001), has the system CA roots for `wss://`, keeps emulator state in the volume `/data` (`KIRBY_OCPP_STATE_DIR=/data`) and exposes port 9000.

The entrypoint starts the emulator by default; naming `kirby-csms` as the first argument starts the central system. `docker run --entrypoint kirby-csms ...` works too.

```bash
# Central system on host port 9000 (profile 0, no TLS)
docker run --rm -p 9000:9000 ghcr.io/archhaze24/kirby-ocpp kirby-csms --listen 0.0.0.0:9000

# Emulator against it (state persists in the named volume)
docker run --rm -v kirby-state:/data ghcr.io/archhaze24/kirby-ocpp \
  --url ws://host.docker.internal:9000/ocpp --id CP-001 --no-tui

# Interactive TUI needs a terminal
docker run --rm -it -v kirby-state:/data ghcr.io/archhaze24/kirby-ocpp --url ws://host.docker.internal:9000/ocpp --id CP-001

# Scenario run, exit code 0 on pass
docker run --rm ghcr.io/archhaze24/kirby-ocpp --url ws://host.docker.internal:9000/ocpp run --scenario charge-session

# Mount your own files read-only (TLS material, scenarios, scripts)
docker run --rm -v "$PWD/certs:/certs:ro" -p 9000:9000 ghcr.io/archhaze24/kirby-ocpp \
  kirby-csms --listen 0.0.0.0:9000 --tls-cert /certs/server.pem --tls-key /certs/server.key
```

Notes:

* Inside a container `kirby-csms` must listen on `0.0.0.0` (its default); publish with `-p HOST:9000`. Bind the host side to `127.0.0.1` (`-p 127.0.0.1:9000:9000`) unless other machines should reach it: at security profile 0 any peer can connect.
* On Linux `host.docker.internal` needs `--add-host host.docker.internal:host-gateway`, or use `--network host` for the emulator.
* The HTTP control API (`--api-listen`) needs a token and TLS for a non-loopback address, which includes `0.0.0.0` inside a container: mount the certificate (`-v "$PWD/certs:/certs:ro"`), pass `--api-listen 0.0.0.0:8080 --api-token-file /certs/api.token --api-tls-cert /certs/server.pem --api-tls-key /certs/server.key` and publish it with `-p 127.0.0.1:8080:8080`. See [api.md](api.md#tls).
* In CI the central system can run from the image instead of npm. A GitHub `services:` container cannot take command arguments, so start it with `docker run -d --name csms -p 127.0.0.1:9000:9000 ghcr.io/archhaze24/kirby-ocpp kirby-csms --listen 0.0.0.0:9000`, wait for `listening on` in `docker logs csms`, and stop it with `docker rm -f csms`. The emulator can run from the same image; the container then needs a route to the CSMS (see above).
* A named volume starts owned by `kirby`. For a bind mount (`-v "$PWD/state:/data"`) the host directory must be writable by uid 10001.
* `--state-dir` and `--no-persist` behave as in [configuration.md](configuration.md#state-directory-and-files).

## Homebrew

```bash
brew install archhaze24/tap/kirby-ocpp
```

Installs both binaries and the manual pages (`man kirby-ocpp`, `man kirby-csms`) on macOS arm64/x64 and Linux arm64/x64 (from the static musl archives). The formula is published with each final release to the tap repo `archhaze24/homebrew-tap` as `Formula/kirby-ocpp.rb`. The template is `packaging/homebrew/kirby-ocpp.rb.in`.

## Scoop

```powershell
scoop bucket add archhaze24 https://github.com/archhaze24/scoop-bucket
scoop install kirby-ocpp
```

Installs both `.exe` files (Windows x64). The manifest is published with each final release to the bucket repo `archhaze24/scoop-bucket` as `bucket/kirby-ocpp.json`. The template is `packaging/scoop/kirby-ocpp.json.in`; `checkver` and `autoupdate` keep the bucket current with new releases.

Both files are produced by `.github/scripts/render-packaging.sh TAG [ASSET_DIR|- [OUT_DIR]]` from the release's `.sha256` assets (read from `ASSET_DIR`, or downloaded from the public release URL when `ASSET_DIR` is omitted or `-`). The release workflow runs it and attaches the result as the workflow artifact `packaging` (not a release asset).

## Build the image from source

Release images use pre-built binaries. To build from a checkout, `Dockerfile.dev` compiles with a `rust:alpine` builder (native musl, slow the first time):

```bash
docker build -f Dockerfile.dev -t kirby-ocpp:dev .
docker run --rm kirby-ocpp:dev --version
```

To assemble the release-style image from your own musl binaries, put them in `dist/linux-amd64/` (or `dist/linux-arm64/`) and run `docker build -t kirby-ocpp:local .`:

```bash
rustup target add x86_64-unknown-linux-musl     # needs musl-tools (musl-gcc)
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked -p kirby-ocpp -p kirby-csms --target x86_64-unknown-linux-musl
mkdir -p dist/linux-amd64
cp target/x86_64-unknown-linux-musl/release/{kirby-ocpp,kirby-csms} dist/linux-amd64/
docker build -t kirby-ocpp:local .
```

`dist/` is git-ignored. `TARGETARCH` selects the directory, so a plain `docker build` on an arm64 host reads `dist/linux-arm64/`.

## Related pages

[README](../README.md) · [Configuration](configuration.md) · [CSMS](csms.md) · [Releasing](../RELEASING.md)
