# Configuration reference

Every command-line flag and environment variable of `kirby-ocpp` and `kirby-csms`, with defaults, ranges, exit codes and the files they touch. The tables follow `--help` of the same version; if they ever disagree, `--help` and `cli.rs` are right and this page is a bug (see [Development](development.md#keeping-docs-in-step-with-code)).

Contents: [Precedence and booleans](#precedence-and-booleans) · [kirby-ocpp](#kirby-ocpp) · [Subcommands](#kirby-ocpp-subcommands) · [Id templates](#id-templates) · [Response rules](#response-rules---rule) · [State directory](#state-directory-and-files) · [kirby-csms](#kirby-csms) · [Exit codes](#exit-codes) · [Related pages](#related-pages)

## Precedence and booleans

* Order: command-line flag, then environment variable, then built-in default. Both binaries use `clap` with `env =`.
* Boolean values (env vars and flags that take `<BOOL>`) accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive. Anything else is a usage error (exit 2). An empty variable counts as unset.
* Pure switches (`--no-persist`, `--tls-skip-verify`, `--show-secrets`, `--json`, `--no-tui`, ...) are set by the flag or by a true value in their variable.
* Numeric options reject out-of-range values with exit 2 (ranges below). Text options reject empty values.

## kirby-ocpp

`kirby-ocpp [OPTIONS] [COMMAND]`. Station options are also accepted after the subcommand. `run --help` and `suite --help` list them and point here; the help of `version`, `completions` and `scenarios` omits them (they are still accepted there and ignored), and `version --help` shows only `--json`. `--url` (or `OCPP_URL`) is the only required value for a real connection; without it the error shows an example and names `kirby-csms --listen 127.0.0.1:9000` as a local peer. `kirby-ocpp --help` groups the flags under the headings Connection, Identity, Station, TLS, Reconnect, Fleet, Faults, Output and Scenarios; the tables below use a similar grouping (the headings differ in places).

### Station and connection

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `-u, --url <URL>` | `OCPP_URL` | none (required) | Central System WebSocket URL, `ws://` or `wss://`, e.g. `ws://localhost:9000/ocpp` |
| `-i, --id <ID>` | `OCPP_CHARGE_POINT_ID` | `CP-001` | Charge point id (non-empty, not `.` or `..`, no control or invisible characters such as newlines, zero-width spaces or bidirectional marks; otherwise exit 2); with `--count` above 1 it is the id template unless `--id-template` is given |
| `--vendor <VENDOR>` | `OCPP_VENDOR` | `archhaze24` | BootNotification vendor (non-empty, at most 20 characters; longer exits 2) |
| `--model <MODEL>` | `OCPP_MODEL` | `KIRBY` | BootNotification model (non-empty, at most 20 characters; longer exits 2) |
| `--serial <SERIAL>` | `OCPP_SERIAL` | none | Serial number in BootNotification, BootNotification schema limit: 25 characters (longer exits 2) |
| `--firmware-version <VERSION>` | `OCPP_FIRMWARE_VERSION` | none | Firmware version in BootNotification, BootNotification schema limit: 50 characters (longer exits 2) |
| `-c, --connector <ID>` | | `1` | Initial connector id (at least 1) |
| `--connectors <COUNT>` | | `1` | Number of connectors (at least 1; raised to `--connector` if smaller) |
| `--heartbeat <SECONDS>` | | `30` | Fallback heartbeat interval (at least 1); the CSMS value from BootNotification wins |
| `--nominal-power-w <WATTS>` | | `7200` | Nominal charging power per connector (at least 1); charging profiles can only lower it |
| `--voltage-v <VOLTS>` | | `230` | Voltage per phase (at least 1), used for A to W conversion and the `Voltage` reading |
| `--phases <COUNT>` | | `1` | Number of phases, 1 to 3 |
| `--ws-subprotocol <PROTOCOL>` | `OCPP_WS_SUBPROTOCOL` | `ocpp1.6` | Requested WebSocket subprotocol (RFC 6455 token characters only) |
| `--ws-ping <SECONDS>` | `OCPP_WS_PING_INTERVAL` | `30` | WebSocket ping interval; `0` disables pings |
| `--call-timeout-ms <MILLISECONDS>` | `OCPP_CALL_TIMEOUT_MS` | `30000` | CALL response timeout, 1 to 86400000 |

### Reconnect

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--reconnect <BOOL>` | `OCPP_RECONNECT` | `true` | Reconnect automatically after an unexpected disconnect |
| `--no-reconnect` | | | Turn reconnect off; wins over `--reconnect` and `OCPP_RECONNECT` |
| `--reconnect-initial-ms <MILLISECONDS>` | `OCPP_RECONNECT_INITIAL_MS` | `1000` | Initial backoff. Exit 2 when reconnect is on, attempts are unlimited and the smaller of this and `--reconnect-max-ms` is `0` |
| `--reconnect-max-ms <MILLISECONDS>` | `OCPP_RECONNECT_MAX_MS` | `30000` | Maximum backoff (see `--reconnect-initial-ms` for the `0` rule) |
| `--reconnect-max-attempts <ATTEMPTS>` | `OCPP_RECONNECT_MAX_ATTEMPTS` | `0` | Maximum attempts; `0` is unlimited |

### TLS and secrets

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--tls-ca <PATH>` | `OCPP_TLS_CA_FILE` | system roots | CA bundle (PEM) for `wss://`; a file that cannot be read exits 2 at startup |
| `--tls-cert <PATH>` | `OCPP_TLS_CERT_FILE` | none | Client certificate for mutual TLS; needs `--tls-key` (one without the other, or a file that cannot be read, exits 2) |
| `--tls-key <PATH>` | `OCPP_TLS_KEY_FILE` | none | Client private key for mutual TLS; needs `--tls-cert` (one without the other, or a file that cannot be read, exits 2) |
| `--tls-server-name <NAME>` | `OCPP_TLS_SERVER_NAME` | URL host | SNI and verification name override |
| `--tls-skip-verify` | `OCPP_TLS_SKIP_VERIFY` | off | Disable certificate verification (local or self-signed testing only) |
| `--show-secrets` | `OCPP_SHOW_SECRETS` | off | Show `AuthorizationKey` values in frame logs; by default they are replaced by `***` |
| `--security-profile-fallback <BOOL>` | `OCPP_SECURITY_PROFILE_FALLBACK` | `true` | Go back to the previous `SecurityProfile` after 3 profile-attributable connection failures (HTTP 401/403, or a TLS alert rejecting the client certificate) with a newly set one, before it ever connected (`A05.FR.08`); see [Security](security.md#emulator-side) |
| `--additional-root-certificate-check` | `OCPP_ADDITIONAL_ROOT_CERTIFICATE_CHECK` | off | Value of the read-only `AdditionalRootCertificateCheck` key: a new Central System root must be signed by the installed one |

Profiles and certificates are explained in [Security](security.md).

### Fleet

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--count <COUNT>` | `OCPP_COUNT` | `1` | Number of stations in one process, 1 to 1000; above 1 the id needs a placeholder |
| `--id-template <TEMPLATE>` | `OCPP_ID_TEMPLATE` | the `--id` value | Id template, see [Id templates](#id-templates) |
| `--first-index <INDEX>` | `OCPP_FIRST_INDEX` | `1` | Index of the first station (the value `{n}` gets) |
| `--stagger-ms <MILLISECONDS>` | `OCPP_STAGGER_MS` | `0` | Delay between starting two stations |

### State and fault injection

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--no-persist` | `KIRBY_OCPP_NO_PERSIST` | off | Do not load or save station state. `run` and `suite` never persist unless `run --persist` is given |
| `--state-dir <PATH>` | `KIRBY_OCPP_STATE_DIR` | `~/.kirby-ocpp/stations` (`%USERPROFILE%` on Windows) | Directory for persisted state, see [State directory](#state-directory-and-files) |
| `--rule <ACTION=BEHAVIOR>` | `KIRBY_OCPP_RULES` | none | Response rule, repeatable; in the variable separate rules with `;`. See [Response rules](#response-rules---rule) |

### Output

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--print-config` | | | Print the effective configuration as JSON and exit. With `--count` above 1 it prints `{"fleet": {...}, "config": {...}}`, `config` being the first station |
| `--no-tui` | | | Never start the terminal UI; log to stdout (automatic when stdout is not a terminal) |
| `--json` | | | One JSON object per line (events and the `run` report); never opens the UI. Never colored |
| `-v, --verbose` | | off | `run` and `suite` also print the event and frame lines (see [Output levels](#output-levels)); excludes `--quiet` |
| `-q, --quiet` | | off | `run` and `suite` print only errors and the final verdict line (see [Output levels](#output-levels)) |
| `--color <WHEN>` | | `auto` | Colorize PASS (green), FAIL (red), SKIP and warnings (yellow) in human output: `auto` (only when stdout is a terminal and `NO_COLOR` is unset or empty), `always`, `never`. Explicit `always` or `never` wins over `NO_COLOR`; the terminal UI is unchanged |
| `--log-file <PATH>` | | none | Also append the event log to this file, with the UI too. A new file is created `0600` on unix (frames hold idTags); an existing file keeps its mode. A helper thread writes it; lines that cannot be written in time are dropped and a `note` line reports the count |
| `--licenses` | | | Print third-party license notices and exit |
| `-V, --version`, `-h, --help` | | | `--version` prints one line; `kirby-ocpp version` prints the build details |

### Output levels

What `run` and `suite` write to stdout (errors always go to stderr):

| Level | `run` | `suite` |
| --- | --- | --- |
| default | The report: `scenario NAME`, one `PASS`/`FAIL`/`SKIP` line per step, `PASSED in N ms` or `FAILED in N ms` (a fleet: one tagged report per station and the aggregate line) | One `PASS`/`FAIL`/`SKIP` line per scenario and the summary line |
| `-v, --verbose` | The event lines (frames and log) as they happen, then the report | The event lines of every scenario between the result lines |
| `-q, --quiet` | Only the verdict line `PASSED in N ms`/`FAILED in N ms` (a fleet: only the aggregate line) | Only the summary line |
| `--json` | Event lines and the report as JSON lines, as before | Event lines, one report per scenario and the suite summary, as before |
| `--json -q` | The report only | The suite summary and the reports, no events |

Plain headless mode (no subcommand) prints every event at any level. `--log-file` always receives the full event log, whatever the level.

## kirby-ocpp subcommands

`kirby-ocpp run --scenario <FILE|NAME>` connects, waits for an accepted BootNotification, runs the scenario and prints the report. A `run` is ephemeral: it neither reads nor writes a state file, so a scenario starts from a clean station every time (`--state-dir` has no effect without `--persist`; `run` prints a note when one is given). It accepts every option above plus:

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--scenario <FILE\|NAME>` | | required | Built-in scenario name or a TOML file |
| `--report <PATH>` | | none | Write the report as JSON. In fleet mode one file per station: `out.CP-001.json` (characters of the id other than letters, digits, `-`, `_` become `_`) |
| `--junit <PATH>` | | none | Write a JUnit XML report; one file, one test suite per station in a fleet |
| `--persist` | | off | Read and write the station state file like a plain station; excludes `--no-persist` (usage error, exit 2) |
| `--timeout <SECONDS>` | | `60` | Give up and fail after this long, connecting included; 1 to 86400. In fleet mode it covers the whole fleet, stagger included |
| `--id-tag <IDTAG>` | | none | Replace the `idTag` of the scenario's `authorize`, `start_tx`, `stop_tx` steps (non-empty) |
| `--var <NAME=VALUE>` | | none | Set a scenario variable (repeatable); overrides `[vars]`; `true`/`false`/numbers keep their type; `station_id` and `index` are built in and cannot be set. See [Scenarios](scenarios.md#variables) |

`kirby-ocpp suite [SOURCE]...` runs several scenarios one after another, each on a fresh station with persistence off (`--state-dir` and `--no-persist` are ignored). It accepts every station option above except a fleet (`--count` above 1 exits 2) plus:

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `[SOURCE]...` | | none | Scenario file, directory (`*.toml` directly inside, sorted) or built-in name; at least one SOURCE or `--all-builtins` is required |
| `--all-builtins` | | off | Also run every built-in scenario except `finish-session` (needs a transaction left by an earlier run) |
| `--report <PATH>` | | none | Write the suite summary as JSON (`"type": "suite_report"`) |
| `--junit <PATH>` | | none | Write one JUnit XML file with one test suite per scenario |
| `--timeout <SECONDS>` | | `60` | Per scenario, connecting included; 1 to 86400 |
| `--fail-fast` | | off | Stop after the first failed scenario; the rest are reported as SKIP |
| `--var <NAME=VALUE>` | | none | Scenario variable for every scenario (repeatable), as for `run` |

Exit codes: [Exit codes](#kirby-ocpp-suite). Details: [Scenarios](scenarios.md#suite-runs).

`kirby-ocpp scenarios list` lists the built-in scenarios; `kirby-ocpp scenarios show <NAME>` prints one as TOML. Scenario format: [Scenarios](scenarios.md).

`kirby-ocpp scenarios validate <FILE|DIR|NAME>...` parses scenarios without connecting (no `--url` needed). It prints `ok PATH (N steps)` per scenario (built-ins as `ok built-in NAME (N steps)`) and `error: PATH: reason` per failure, scanning directories like `suite` does (every `*.toml` directly inside, sorted). Exit 0 when all are valid, 2 when any is not.

`kirby-ocpp version` prints the tool version, the git commit, the build date, the target triple and the supported protocols (`OCPP 1.6J`); with `--json` one JSON object with the keys `name`, `version`, `commit`, `build_date`, `target` and `protocols`. The commit is `unknown` in a build without git. `kirby-ocpp --version` stays a single line.

`kirby-ocpp completions <bash|zsh|fish|powershell|elvish>` prints a shell completion script to stdout; installation hints are in [emulator.md](emulator.md#shell-completions).

## Id templates

Used by `--id` and `--id-template`.

* `{n}` is the station index, `{n:03}` the index zero-padded to 3 digits. `{{` and `}}` are literal braces.
* The first index is `--first-index`; `--count 3 --first-index 10` gives `CP-10`, `CP-11`, `CP-12` for `CP-{n}`.
* With `--count` above 1 a template without a placeholder is a usage error (exit 2).
* `--count 1` without `--id-template` uses `--id` as given; with an explicit `--id-template` the template is expanded.
* Output in fleet mode is tagged `[CP-001] ...`, and every JSON line gets a `station` field. With `--count 1` nothing is tagged.

## Response rules (`--rule`)

Makes the station misbehave on purpose when the CSMS sends a command. Form: `ACTION=BEHAVIOR[*COUNT]`. `ACTION` is the name of one CSMS command the station handles (case-insensitive). `*COUNT` limits the rule to that many CALLs.

| Behavior | Effect |
| --- | --- |
| `reject` | The action's own rejection status, otherwise CALLERROR `NotSupported` (for actions without a rejection status such as `GetConfiguration`) |
| `delay:MS` | Handle normally, reply `MS` ms later (0 to 3600000) |
| `error:CODE[:TEXT]` | CALLERROR with an OCPP-J error code, optional description text |
| `drop` | Close the connection without replying; the reconnect lifecycle follows (off with `--no-reconnect`) |
| `ignore` | Never reply, so the CSMS call timeout fires |

Repeat the flag or separate rules with `;` (also in `KIRBY_OCPP_RULES`); a rule must not contain `;`. Examples:

```bash
--rule Reset=reject --rule GetConfiguration=delay:5000 --rule ChangeConfiguration=error:InternalError*3
```

Rules can also be changed at run time from the TUI and from scenarios (`set_rule`, `clear_rules`).

## State directory and files

* Directory: `--state-dir`, else `~/.kirby-ocpp/stations`. `--no-persist` disables loading and saving.
* One file per charge point id: `<id>.station.json` (versioned, written atomically: temp file, rename, directory fsync, mode `0600`). Secrets live in a separate `<id>.station.secrets.json`.
* Ids are escaped for the filesystem. A name that would exceed 100 bytes is shortened with a hash suffix (`...~<16 hex>.station.json`).
* A corrupt file or a file belonging to another charge point is moved to `<file>.bak` and the station starts clean. Format changes require a `STORE_VERSION` bump (see [Development](development.md#store-version)).
* Fleet stations never collide: the file is named after each station's id.
* `kirby-csms` keeps no state on disk; its station records live in memory.

## kirby-csms

`kirby-csms [OPTIONS]`: TUI on a terminal, otherwise headless. Station id is the last path segment of the URL: `ws(s)://HOST:PORT/<any prefix>/<station id>`.

### Listener and security

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--listen <ADDR>` | `KIRBY_CSMS_LISTEN` | `0.0.0.0:9000` | Address to listen on; port `0` picks a free port (the real address is printed first) |
| `--api-listen <ADDR>` | `KIRBY_CSMS_API_LISTEN` | off | Address of the local HTTP control API; port `0` picks a free port. A non-loopback address needs a token (exit 2 otherwise). See [api.md](api.md) |
| `--api-token <TOKEN>` | `KIRBY_CSMS_API_TOKEN` | none | Bearer token the API requires (visible in `ps`: prefer the environment variable or `--api-token-file`); needs `--api-listen`, excludes `--api-token-file` |
| `--api-token-file <FILE>` | `KIRBY_CSMS_API_TOKEN_FILE` | none | File holding the API bearer token (read once, trailing newline dropped, must not be empty; a warning on unix when group/world readable); needs `--api-listen`, excludes `KIRBY_CSMS_API_TOKEN`; a token is visible ASCII without spaces |
| `--tls-cert <FILE>` | `KIRBY_CSMS_TLS_CERT` | none | Server certificate chain (PEM) for `wss://`; needs `--tls-key` |
| `--tls-key <FILE>` | `KIRBY_CSMS_TLS_KEY` | none | Server private key (PEM); needs `--tls-cert` |
| `--tls-client-ca <FILE>` | `KIRBY_CSMS_TLS_CLIENT_CA` | none | Trusted CA(s) for client certificates; with `--tls-cert` every client must present one (mutual TLS); needs `--tls-cert` |
| `--security-profile <0..3>` | `KIRBY_CSMS_SECURITY_PROFILE` | `0` | `0` none, `1` Basic Auth, `2` Basic Auth plus TLS, `3` TLS with client certificates (CN must equal the station id) |
| `--auth-key <ID=KEY>` | `KIRBY_CSMS_AUTH_KEY` (`ID=KEY;ID=KEY`) | none | Basic Auth key of one station (repeatable). The environment variable is used only when no `--auth-key` is given. The flag value is visible in `ps`; prefer the variable or `--auth-keys-file` |
| `--auth-keys-file <FILE>` | `KIRBY_CSMS_AUTH_KEYS_FILE` | none | Lines `ID=KEY`; blank lines and `#` comments are skipped. A leading UTF-8 BOM is ignored, leading spaces and the line end are dropped, trailing spaces belong to the key. At most 1 MiB. A startup warning is printed when the file is readable by group or others (Unix) |
| `--default-auth-key <KEY>` | `KIRBY_CSMS_DEFAULT_AUTH_KEY` | none | Key for stations without an `ID=KEY` entry |
| `--subprotocol <POLICY>` | `KIRBY_CSMS_SUBPROTOCOL` | `require:ocpp1.6` | `require:NAME` (refuse clients that do not offer it), `select:NAME` (always select it), `omit` (select nothing), `reject` (refuse every upgrade) |
| `--show-secrets` | `KIRBY_CSMS_SHOW_SECRETS` | off | Keep `AuthorizationKey` values in frame logs and history; by default replaced by `***` |

### Behavior

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--transaction-ids <POLICY>` | `KIRBY_CSMS_TRANSACTION_IDS` | `fixed:123` | StartTransaction ids: `fixed:ID` (every transaction gets `ID`) or `incrementing:FIRST` |
| `--heartbeat-interval <SECONDS>` | `KIRBY_CSMS_HEARTBEAT_INTERVAL` | `300` | `interval` in the BootNotification answer (at least 1) |
| `--validate-inbound <BOOL>` | `KIRBY_CSMS_VALIDATE_INBOUND` | `true` | Validate inbound CALLs against the OCPP 1.6J schemas; an invalid one gets a CALLERROR instead of the canned reply. A scripted fault on the same action still wins; unknown actions are not checked |
| `--ping-interval <SECONDS>` | `KIRBY_CSMS_PING_INTERVAL` | `30` | WebSocket ping to each station this often; `0` disables the watchdog |
| `--pong-timeout <SECONDS>` | `KIRBY_CSMS_PONG_TIMEOUT` | `10` | Drop a station that sends nothing this long after a ping (disconnect reason `ping_timeout`); at least 1. A non-zero `--ping-interval` below this value is a usage error (exit 2) |
| `--call-timeout <SECONDS>` | `KIRBY_CSMS_CALL_TIMEOUT` | `30` | How long a command to a station may wait for its answer, 1 to 86400 |
| `--history-cap <COUNT>` | `KIRBY_CSMS_HISTORY_CAP` | `1000` | Frames kept per station; `0` keeps none |
| `--transaction-history-cap <COUNT>` | `KIRBY_CSMS_TRANSACTION_HISTORY_CAP` | `100` | Stopped transactions kept per station; `0` keeps none |

### Output, input and scripts

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--json` | `KIRBY_CSMS_JSON` | off | One JSON object per line |
| `--log-file <PATH>` | `KIRBY_CSMS_LOG_FILE` | none | Append every event as a JSON line (new file `0600` on unix, an existing file keeps its mode) (helper thread; late lines are dropped and a `note` line counts them) |
| `--stdin` | `KIRBY_CSMS_STDIN` | auto | Read JSON-line commands from stdin even on a terminal (automatic when stdin is not a terminal and there is no `--script`) |
| `--no-tui` | `KIRBY_CSMS_NO_TUI` | auto | Never open the UI, print events to stdout (automatic when stdout or stdin is not a terminal, and with `--json`, `--script` or `--stdin`) |
| `--script <FILE>` | `KIRBY_CSMS_SCRIPT` | none | Run a TOML script against connecting stations, print the report, exit |
| `--report <PATH>` | | none | Script report as JSON; needs `--script` |
| `--junit <PATH>` | | none | Script report as JUnit XML; needs `--script` |
| `--var <NAME=VALUE>` | | none | Set a script variable (repeatable): `${NAME}` in the script becomes VALUE. Overrides `[vars]`; `true`/`false`/numbers keep their type; needs `--script` |
| `--timeout <SECONDS>` | | `60` | Fail the script after this long, 1 to 86400; needs `--script` |
| `--licenses` | | | Print third-party license notices and exit |
| `-V, --version`, `-h, --help` | | | |

CSMS usage and the script format: [CSMS](csms.md), [Scenarios](scenarios.md). The HTTP API: [api.md](api.md).

## Exit codes

This section is the single reference for exit codes; other pages link here. Code `2` always means a usage or configuration error found before the run starts (bad flag, bad value, invalid scenario or script file). Code `130` is SIGINT; SIGTERM ends a run with `1`.

### kirby-ocpp, plain station

No subcommand: the terminal UI, or headless mode (`--no-tui`, `--json`, no terminal). A fleet (`--count` above 1) follows the same rules.

| Code | Meaning |
| --- | --- |
| 0 | Normal shutdown (quit in the UI, SIGINT or SIGTERM in headless mode). `--print-config`, `--licenses`, `version`, `completions` and `scenarios list|show` also exit 0 |
| 1 | Headless only: the connection was lost for good (`--no-reconnect`, or `--reconnect-max-attempts` used up). The process prints `reconnect stopped (...)` with the last connect error and a hint. A fleet exits 1 when every station gave up. `run` and `suite` also exit 1 with `connection lost: ...` when that happens while a scenario is running. The UI keeps running instead |
| 2 | Usage or configuration error |

### kirby-ocpp run

`run --scenario`, one station or a fleet.

| Code | Meaning |
| --- | --- |
| 0 | Every step passed (fleet: every station passed) |
| 1 | A step failed, the run timed out (`--timeout`), the station could not connect or boot, the connection was lost for good, or a report file could not be written (fleet: any station failed) |
| 2 | Usage or configuration error, including an invalid scenario file or an unknown `--var` |
| 130 | SIGINT. No JUnit file is written (fleet: no report files). SIGTERM exits 1 and still writes the single-station JUnit file |

### kirby-ocpp suite

| Code | Meaning |
| --- | --- |
| 0 | Every scenario passed |
| 1 | Any scenario failed, timed out or could not boot, or a report file could not be written |
| 2 | Usage error: no source, empty directory, invalid scenario file, `--count` above 1 |
| 130 | SIGINT. The running scenario ends as an `error` row (`interrupted`), the rest are `skipped`, and `--report` and `--junit` are written for what finished. SIGTERM does the same and exits 1 |

### kirby-ocpp scenarios validate

| Code | Meaning |
| --- | --- |
| 0 | Every file is valid |
| 2 | At least one file is not |

### kirby-csms, plain server

| Code | Meaning |
| --- | --- |
| 0 | Clean shutdown |
| 1 | Runtime error, for example the listener could not bind |
| 2 | Usage or configuration error. Invalid flag combinations are found before the server binds |

### kirby-csms --script

| Code | Meaning |
| --- | --- |
| 0 | The script passed |
| 1 | A step failed or timed out (`--timeout`), the server could not start, or a report file could not be written. On SIGTERM the `--junit` file still gets one failed suite (`interrupted`) |
| 2 | Usage, configuration or script error |
| 130 | SIGINT, no report |

## Related pages

[Emulator](emulator.md) · [CSMS](csms.md) · [Scenarios](scenarios.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
