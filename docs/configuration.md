# Configuration reference

Every command-line flag and environment variable of `kirby-ocpp` and `kirby-csms`, with defaults, ranges, exit codes and the files they touch. The tables follow `--help` of the same version; if they ever disagree, `--help` and `cli.rs` are right and this page is a bug (see [Development](development.md#keeping-docs-in-step-with-code)).

Contents: [Precedence and booleans](#precedence-and-booleans) · [kirby-ocpp](#kirby-ocpp) · [Subcommands](#kirby-ocpp-subcommands) · [Configuration file](#configuration-file) · [kirby-csms configuration file](#kirby-csms-configuration-file) · [Id templates](#id-templates) · [Response rules](#response-rules---rule) · [State directory](#state-directory-and-files) · [kirby-csms](#kirby-csms) · [Exit codes](#exit-codes) · [Related pages](#related-pages)

## Precedence and booleans

* Order: command-line flag, then environment variable, then [configuration file](#configuration-file) (`--config`), then built-in default. Both binaries use `clap` with `env =`.
* Boolean values (env vars and flags that take `<BOOL>`) accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive. Anything else is a usage error (exit 2). An empty variable counts as unset.
* Pure switches (`--no-persist`, `--tls-skip-verify`, `--show-secrets`, `--json`, `--no-tui`, ...) are set by the flag or by a true value in their variable.
* Numeric options reject out-of-range values with exit 2 (ranges below). Text options reject empty values.

## kirby-ocpp

`kirby-ocpp [OPTIONS] [COMMAND]`. Station options are also accepted after the subcommand. `run --help`, `suite --help` and `check --help` list them and point here; the help of `version`, `completions` and `scenarios` omits them (they are still accepted there and ignored), `version --help` shows only `--json`, and `state --help` shows only `--id`, `--state-dir` and `--json`. `--url` (or `OCPP_URL`) is the only required value for a real connection; without it the error shows an example and names `kirby-csms --listen 127.0.0.1:9000` as a local peer. `kirby-ocpp --help` groups the flags under the headings Connection, Identity, Station, TLS, Reconnect, Fleet, Faults, Configuration, Output and Scenarios; the tables below use a similar grouping (the headings differ in places).

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
| `--print-config[=<FORMAT>]` | | | Print the effective configuration and exit without connecting. Without a value or with `=json`: JSON; with `--count` above 1 it prints `{"fleet": {...}, "config": {...}}`, `config` being the first station. With `=toml`: a configuration file that is valid input for `--config` (see [Configuration file](#configuration-file)); secrets are never printed. `FORMAT` is `json` or `toml`, and the value needs `=` |
| `--no-tui` | | | Never start the terminal UI; log to stdout (automatic when stdout is not a terminal) |
| `--json` | | | One JSON object per line (events and the `run` report); never opens the UI. Never colored |
| `-v, --verbose` | | off | `run` and `suite` also print the event and frame lines (see [Output levels](#output-levels)); excludes `--quiet` |
| `-q, --quiet` | | off | `run` and `suite` print only errors and the final verdict line (see [Output levels](#output-levels)) |
| `--color <WHEN>` | | `auto` | Colorize PASS (green), FAIL (red), SKIP and warnings (yellow) in human output: `auto` (only when stdout is a terminal and `NO_COLOR` is unset or empty), `always`, `never`. Explicit `always` or `never` wins over `NO_COLOR`; the terminal UI is unchanged |
| `--log-file <PATH>` | | none | Also append the event log to this file, with the UI too. A new file is created `0600` on unix (frames hold idTags); an existing file keeps its mode. A helper thread writes it; lines that cannot be written in time are dropped and a `note` line reports the count |
| `--licenses` | | | Print third-party license notices and exit |
| `-V, --version`, `-h, --help` | | | `--version` prints one line; `kirby-ocpp version` prints the build details |

### Configuration

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--config <FILE>` | `KIRBY_OCPP_CONFIG` | none | Read settings from a TOML [configuration file](#configuration-file). Flags and environment variables win over the file; the file wins over defaults |
| `--profile <NAME>` | `KIRBY_OCPP_PROFILE` | none | Overlay the `[profile.NAME]` tables of the configuration file on its top-level values; needs `--config` |

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
| `--report <PATH>` | | none | Write the report as JSON. In fleet mode one file per station: `out.CP-001.json` (characters of the id other than letters, digits, `-`, `_` become `_`). See [Report files](#report-files) |
| `--junit <PATH>` | | none | Write a JUnit XML report; one file, one test suite per station in a fleet. See [Report files](#report-files) |
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
| `--filter <GLOB>` | | none | Run only scenarios whose name matches the glob (`*` any run of characters, `?` one character; repeatable, any match selects) |
| `--skip <GLOB>` | | none | Leave out scenarios whose name matches the glob (repeatable); applied after `--filter` |
| `--tag <TAG>` | | none | Run only scenarios that carry the tag (repeatable: a scenario needs every given tag) |
| `--skip-tag <TAG>` | | none | Leave out scenarios that carry the tag (repeatable: any given tag leaves a scenario out) |
| `--retries <N>` | | `0` | Run a failed scenario again up to N times, each on a fresh station; 0 to 10. A pass after a failure is reported as flaky and counts as passed |
| `--jobs <N>` | | `1` | Run up to N scenarios at the same time; 1 to 16. Slot 2 and up append `-2`, `-3`, ... to the station id |
| `--github-annotations` | | off | Print a GitHub Actions `::error` (failed) or `::warning` (flaky) workflow command per scenario; exit 2 together with `--json` |
| `--summary-md <PATH>` | | none | Write a Markdown summary table; the table is also appended to the file named by `$GITHUB_STEP_SUMMARY` when that variable is set, with or without the flag |

None of the `suite` flags is a configuration file key. Exit codes: [Exit codes](#kirby-ocpp-suite). Details: [Scenarios](scenarios.md#suite-runs).

`kirby-ocpp check` tests the way to the Central System one step at a time and prints a checklist. It reads the same connection options as a station: `--url` (required), `--id`, `--ws-subprotocol`, `--call-timeout-ms`, the TLS flags (`--tls-ca`, `--tls-cert`, `--tls-key`, `--tls-server-name`, `--tls-skip-verify`), the identity flags that go into the BootNotification (`--vendor`, `--model`, `--serial`, `--firmware-version`), `--json` and `--color`; Basic Auth is taken from `user:password@` in the URL. It opens one connection, sends exactly one BootNotification and closes; it never reads or writes the station state. A fleet (`--count` above 1) exits 2. Plus:

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--timeout <SECONDS>` | | `30` | Give up after this long in total; 1 to 3600. One connection step (DNS, TCP, TLS, upgrade) is also limited to 10 seconds, and the BootNotification answer to `--call-timeout-ms`, always within what is left of the total |

The steps, in order: `url` (the WebSocket URL with the id appended; scheme `ws` or `wss`), `dns` (the addresses of the host), `tcp` (connect to the first reachable address), `tls` (`wss` only: version, certificate subject and issuer, expiry, how the certificate was verified), `websocket` (the upgrade with the subprotocol: status, the subprotocol the server selected), `boot` (the BootNotification: status, interval, clock difference) and `close` (the close handshake). A failed step skips the steps that depend on it. Each line starts with one of `PASS`, `WARN`, `FAIL` or `SKIP`; a failed step may be followed by a `hint:` line. The last line is `PASSED` or `FAILED` with the counts. Judgments: a server certificate that expires in under 14 days is a warning; an expired one fails the step (with `--tls-skip-verify` it is a warning, since the connection works); a `Pending` boot is a warning and a `Rejected` boot fails; a server clock that differs from this machine by more than 60 seconds is a warning. A `wss` handshake that fails certificate verification also prints the subject and issuer the server presented. The password of the URL and the values of its query parameters never appear in the output (`***`), and text that comes from the server is stripped of control characters.

`check --json` prints one JSON object instead of the lines:

| Key | Value |
| --- | --- |
| `schema` | `1` |
| `ocpp_version` | `"1.6"` |
| `tool_version` | Version of the tool |
| `url` | The URL that was checked, with the id, the password and the query values replaced by `***` |
| `status` | `"passed"` unless a step failed, then `"failed"` |
| `steps` | An array in the order above; each step has `name` (`url`, `dns`, `tcp`, `tls`, `websocket`, `boot`, `close`), `status` (`passed`, `warning`, `failed`, `skipped`), `detail` (text), `hint` (text, or `null`) and `duration_ms` (integer) |

New keys may be added to these objects; none is renamed or removed. None of the `check` flags is a configuration file key. Exit codes: [Exit codes](#kirby-ocpp-check). Example output: [Emulator](emulator.md#connection-check).

`kirby-ocpp state path|show|reset` works on the persisted state of a station (see [State directory and files](#state-directory-and-files)). It never connects and needs no `--url`. The directory is `--state-dir` (`KIRBY_OCPP_STATE_DIR`, or the `state_dir` key of the configuration file), else `~/.kirby-ocpp/stations`; the station is `--id`.

| Command | Meaning |
| --- | --- |
| `state path [--id ID]` | Print the state directory; with `--id` the state file of that station. Nothing is created or read. `--json`: `{"schema": 1, "dir", "station", "file", "secrets_file"}` (`station`, `file` and `secrets_file` are `null` without `--id`) |
| `state show --id ID` | Print what the state file holds: store version, availability, connectors with their transaction, local list, authorization cache, charging profiles, offline queue, certificates, queued security events and the configuration keys. The secrets file is never read: only its presence is reported, and configuration values whose key name contains `AuthorizationKey`, `password`, `secret` or `privateKey` print as `***`. A file that cannot be used (corrupt, or of another station) is reported and left where it is. `--json`: `{"schema": 1, "station", "file", "status", "store_version", "supported_store_version", "secrets_file" (boolean), "state"}` with `status` `found`, `missing` or `unusable` (`unusable` adds `reason`; `state` is `null` unless found) |
| `state reset --id ID [--yes]` | Delete the state file, the secrets file and their `.bak` copies of that station, nothing else and nothing outside the state directory. Asks `Delete? [y/N]` on a terminal; without a terminal it needs `-y, --yes` (exit 2 without it). Nothing to delete is not an error. `--json`: `{"schema": 1, "station", "deleted": [paths]}` |

`state show` and `state reset` need the id to be given (`--id`, `OCPP_CHARGE_POINT_ID` or the `id` key of the configuration file); the default id `CP-001` is not assumed. A state or secrets file written by a newer version is refused by `show` and `reset` and stays untouched. None of the `state` flags is a configuration file key. Exit codes: [Exit codes](#kirby-ocpp-state).

`kirby-ocpp scenarios list` lists the built-in scenarios; `kirby-ocpp scenarios show <NAME>` prints one as TOML. Scenario format: [Scenarios](scenarios.md).

`kirby-ocpp scenarios validate <FILE|DIR|NAME>...` parses scenarios without connecting (no `--url` needed). It prints `ok PATH (N steps)` per scenario (built-ins as `ok built-in NAME (N steps)`) and `error: PATH: reason` per failure, scanning directories like `suite` does (every `*.toml` directly inside, sorted). Exit 0 when all are valid, 2 when any is not.

`kirby-ocpp version` prints the tool version, the git commit, the build date, the target triple and the supported protocols (`OCPP 1.6J`); with `--json` one JSON object with the keys `name`, `version`, `commit`, `build_date`, `target` and `protocols`. The commit is `unknown` in a build without git. `kirby-ocpp --version` stays a single line.

`kirby-ocpp completions <bash|zsh|fish|powershell|elvish>` prints a shell completion script to stdout; installation hints are in [emulator.md](emulator.md#shell-completions).

### Report files

`run`, `suite` and a fleet `run` check `--report` and `--junit` before anything connects, and exit 2 with a one-line message naming the flag when:

* the directory of the path does not exist or does not accept a new file, or the path is a directory;
* `--report` and `--junit` name the same file;
* a path is one of the scenario files being run (it would be overwritten).

Each file is written to a temporary file in the same directory and renamed over the target, so a reader never sees a half-written report. An existing file keeps its permissions. A write that fails after the run (a full disk) still exits 1 with `cannot write the report`.

## Configuration file

`kirby-ocpp --config FILE` (or `KIRBY_OCPP_CONFIG`) reads settings from a TOML file, so a test setup lives in one versionable file instead of a long command line. `--profile NAME` (or `KIRBY_OCPP_PROFILE`) picks a named variant inside the file. Both are global flags: they work for the plain station and for `run` and `suite`. `version`, `completions` and `scenarios` never read the file. `check` takes its connection settings from it; `state` takes `id` and `state_dir`.

### Precedence

For every setting: command-line flag, then environment variable, then the selected profile, then the top level of the file, then the built-in default. A flag or variable that is set always wins, also over a file value that is stricter or looser. A list setting (`rule`) is replaced as a whole: a `--rule` flag or `KIRBY_OCPP_RULES` discards the `rule` list of the file, and a `rule` list of the profile discards the one of the top level. `--no-reconnect`, `--verbose` and `--quiet` keep their exclusion rules: a file value is dropped when the flag or variable that excludes it is set.

### Format

* TOML, at most 1 MiB. A file that cannot be read, is too large, is not valid TOML or breaks any rule below exits 2 with a message that names the file and, where it applies, the key path (`connection.url`, `profile.lab.tls.tls_ca`).
* Top-level `schema = 1` is optional; no other value is accepted. Top-level `protocol = "ocpp1.6"` is optional; no other value is accepted. The name `protocol` is reserved for the OCPP version of the file; synonyms such as `ocpp_version` are rejected as unknown keys. Both keys are accepted only at the top level.
* Settings sit in tables. The tables match the headings of `kirby-ocpp --help`, in lower case: `[connection]`, `[identity]`, `[station]`, `[tls]`, `[reconnect]`, `[fleet]`, `[faults]` and `[output]`.
* The key of a setting is the long flag name with `-` replaced by `_`: `--tls-ca` is `tls_ca`, `--reconnect-max-ms` is `reconnect_max_ms`. The key keeps its table even when the table name repeats a prefix (`[tls]` holds `tls_ca`). Values have the type of the flag: text, integer, boolean (`true` or `false`), or an array of text for `rule`. Text values are not trimmed or parsed any differently from the same flag; a value that the flag would refuse (out of range, empty, wrong form) is refused with the same message.
* Unknown tables and unknown keys are errors, never ignored. A key in the wrong table is an unknown key.
* Settings that exist for one OCPP version only belong to a version table below the section, such as `[station.v16]`. No setting needs one today; the table name is reserved and holds no keys yet.
* Not file keys: `--config`, `--profile`, `--print-config`, `--licenses` and the arguments of `run`, `suite`, `check`, `state` and `scenarios` (`--scenario`, `--report`, `--junit`, `--var`, `--yes`, ...). They describe one invocation, not a station.
* Relative paths in the file (`tls_ca`, `tls_cert`, `tls_key`, `state_dir`, `log_file`) resolve against the directory of the configuration file, not against the working directory. Paths given by flag or variable keep resolving against the working directory.

| Table | Keys |
| --- | --- |
| `[connection]` | `url`, `ws_subprotocol`, `ws_ping`, `call_timeout_ms` |
| `[identity]` | `id`, `vendor`, `model`, `serial`, `firmware_version` |
| `[station]` | `connector`, `connectors`, `heartbeat`, `nominal_power_w`, `voltage_v`, `phases`, `no_persist`, `state_dir` |
| `[tls]` | `tls_ca`, `tls_cert`, `tls_key`, `tls_server_name`, `tls_skip_verify`, `additional_root_certificate_check`, `security_profile_fallback` |
| `[reconnect]` | `reconnect`, `no_reconnect`, `reconnect_initial_ms`, `reconnect_max_ms`, `reconnect_max_attempts` |
| `[fleet]` | `count`, `id_template`, `first_index`, `stagger_ms` |
| `[faults]` | `rule` (array of `ACTION=BEHAVIOR[*COUNT]` strings, see [Response rules](#response-rules---rule)) |
| `[output]` | `show_secrets`, `no_tui`, `json`, `log_file`, `verbose`, `quiet`, `color` |

The meaning, default and range of each key are those of the flag with the same name in the tables above. Pure switches (`tls_skip_verify`, `no_persist`, `show_secrets`, ...) take `true` or `false`; `false` leaves the switch off.

### Profiles

`[profile.NAME]` tables hold the same tables as the top level (`[profile.NAME.connection]`, `[profile.NAME.tls]`, ...). A profile has no `schema`, `protocol` or nested `profile`. Without `--profile` only the top level applies. With `--profile NAME` every key of the profile replaces the key of the top level; keys the profile does not set keep the top-level value. All profiles are checked on every load, not only the selected one. An unknown name exits 2 and lists the profiles the file defines. `--profile` without `--config` exits 2.

### Secrets

A configuration file never carries a secret. A key that would hold one is a configuration error that names the key and the allowed alternatives (a `*_file` key naming a file with the secret, or the environment variable). The only such value of `kirby-ocpp` is the password in the URL: a `url` such as `ws://user:password@host/path` is rejected in the file, and the message does not repeat the password. Pass that URL with `--url` or `OCPP_URL`. The `AuthorizationKey` of a station is set by the CSMS and lives in the state directory, not in the configuration. Certificate and key settings (`tls_ca`, `tls_cert`, `tls_key`) are paths to files, which is allowed.

`--print-config=toml` omits the password of a URL and never reads or prints the content of the files that paths point to.

### `--print-config=toml`

Prints the effective configuration (flags, environment, file and defaults merged, the selected profile applied, no `[profile]` tables) as a configuration file and exits 0 without connecting. Settings without a value (`serial`, `state_dir`, ...) are left out. Paths are printed absolute. The output is valid input for `--config`, and printing it again gives the same text:

```bash
kirby-ocpp --config lab.toml --profile staging --print-config=toml > effective.toml
kirby-ocpp --config effective.toml --print-config=toml | diff - effective.toml
```

A fleet prints its settings in `[fleet]`. The JSON form (`--print-config` without a value, or `=json`) keeps its shape and reflects the file and the profile like any other command.

### Example

```toml
schema = 1
protocol = "ocpp1.6"

[connection]
url = "ws://localhost:9000/ocpp"
call_timeout_ms = 15000

[identity]
id = "CP-001"
vendor = "Acme"
model = "AC-22"

[station]
connectors = 2
state_dir = "state"            # relative to the directory of this file

[reconnect]
reconnect_max_attempts = 5

[fleet]
count = 10
id_template = "CP-{n:03}"
stagger_ms = 200

[faults]
rule = ["Reset=reject", "GetConfiguration=delay:2000"]

[output]
no_tui = true

[profile.staging.connection]
url = "wss://staging.example/ocpp"

[profile.staging.tls]
tls_ca = "certs/staging-ca.pem"
```

`kirby-ocpp --config lab.toml` runs the top level; `kirby-ocpp --config lab.toml --profile staging` connects to the staging URL with its CA, everything else unchanged; `--count 3` on the command line wins over `count = 10`.

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
* `kirby-ocpp state path`, `state show` and `state reset` print the location, summarize the content and delete the files of one station; see [kirby-ocpp subcommands](#kirby-ocpp-subcommands).
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
| `--default-auth-key <KEY>` | `KIRBY_CSMS_DEFAULT_AUTH_KEY` | none | Key for stations without an `ID=KEY` entry; excludes `--default-auth-key-file` |
| `--default-auth-key-file <FILE>` | `KIRBY_CSMS_DEFAULT_AUTH_KEY_FILE` | none | File holding the default key (read once, trailing line end dropped, must not be empty, at most 64 KiB; a warning on unix when group/world readable); excludes `--default-auth-key` |
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

### Configuration

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--config <FILE>` | `KIRBY_CSMS_CONFIG` | none | Read settings from a TOML [configuration file](#kirby-csms-configuration-file). Flags and environment variables win over the file; the file wins over defaults |
| `--profile <NAME>` | `KIRBY_CSMS_PROFILE` | none | Overlay the `[profile.NAME]` tables of the configuration file on its top-level values; needs `--config` |
| `--print-config[=<FORMAT>]` | | off | Print the effective configuration and exit 0 without binding a port. `json` (the default form) or `toml`, a file that `--config` accepts. Inline secrets are never printed and no file the settings point to is read; excludes `--check-config` |
| `--check-config` | | off | Validate the configuration file, the selected profile and every flag, including the key files, the certificates and the script, then print one `OK:` line and exit 0, or print the error and exit 2. Never binds a port |

CSMS usage and the script format: [CSMS](csms.md), [Scenarios](scenarios.md). The HTTP API: [api.md](api.md).

## kirby-csms configuration file

`kirby-csms --config FILE` (or `KIRBY_CSMS_CONFIG`) reads settings from a TOML file, so a server setup with its stations and response rules lives in one versionable file. `--profile NAME` (or `KIRBY_CSMS_PROFILE`) picks a named variant inside the file. The format follows the [emulator's configuration file](#configuration-file): the same file rules, `schema`, `protocol`, profiles, error style and exit codes.

### Precedence and format

For every setting: command-line flag, then environment variable, then the selected profile, then the top level of the file, then the built-in default. A flag or variable that is set always wins. `api_token_file` and `default_auth_key_file` of the file are dropped when `--api-token` / `KIRBY_CSMS_API_TOKEN` or `--default-auth-key` / `KIRBY_CSMS_DEFAULT_AUTH_KEY` is set, so the file never turns a set value into a conflict. A flag may be satisfied by the file: `--api-token-file` with `api_listen` in the file, or `--tls-cert` with `tls_key` in the file, is valid.

* TOML, at most 1 MiB; a problem exits 2 with a message that names the file and the key path (`behavior.history_cap`, `profile.lab.rule[1].behavior`, `station[0].id`).
* Top-level `schema = 1` and `protocol = "ocpp1.6"` are optional; no other value is accepted. Both are valid only at the top level.
* Settings sit in tables named after the groups of `kirby-csms --help`, in lower case. The key of a setting is the long flag name with `-` replaced by `_`. Values have the type of the flag (text, integer, boolean); a value the flag would refuse is refused with the same message, and the message points at the key.
* Unknown tables and keys are errors. A key in the wrong table is an unknown key.
* Relative paths in the file resolve against the directory of the file; paths given by flag or variable resolve against the working directory.
* Settings that exist for one OCPP version only belong to a version table below the section (reserved, none yet) or to a block with its own `protocol` key.
* Not file keys: `--config`, `--profile`, `--print-config`, `--check-config`, `--licenses` and the script arguments `--script`, `--report`, `--junit`, `--var`, `--timeout`. They describe one invocation, not a server.

| Table | Keys |
| --- | --- |
| `[listener]` | `listen`, `subprotocol` |
| `[api]` | `api_listen`, `api_token_file` |
| `[tls]` | `tls_cert`, `tls_key`, `tls_client_ca` |
| `[security]` | `security_profile`, `auth_keys_file`, `default_auth_key_file`, `show_secrets` |
| `[behavior]` | `transaction_ids`, `heartbeat_interval`, `validate_inbound`, `ping_interval`, `pong_timeout`, `call_timeout`, `history_cap`, `transaction_history_cap` |
| `[output]` | `json`, `log_file`, `stdin`, `no_tui` |

### Secrets

A configuration file never carries a secret. The keys `api_token`, `auth_key` and `default_auth_key` exist only to refuse an inline value: the error names the key and the alternative, and never repeats the value.

| Secret | In the file | Elsewhere |
| --- | --- | --- |
| API bearer token | `api_token_file` | `--api-token`, `KIRBY_CSMS_API_TOKEN`, `--api-token-file` |
| Station keys | `auth_keys_file` (lines `ID=KEY`), or `auth_key_file` of a `[[station]]` block (the file holds one key) | `--auth-key`, `KIRBY_CSMS_AUTH_KEY` |
| Default key | `default_auth_key_file` (the file holds one key) | `--default-auth-key`, `KIRBY_CSMS_DEFAULT_AUTH_KEY`, `--default-auth-key-file` |

Key files are read at start (and by `--check-config`), never by `--print-config`. Keep them private: a startup warning appears when a key file is readable by group or others (Unix). `--print-config` leaves inline secrets out and says so in a comment; it prints the paths of the key files.

### Blocks

`[[station]]` and `[[rule]]` are arrays of tables for settings that belong to one station or one rule. Every block has the optional key `protocol = "ocpp1.6"` (the only accepted value), so a later release can scope a block to one OCPP version. An array in `[[profile.NAME.station]]` or `[[profile.NAME.rule]]` replaces the array of the top level as a whole when the profile is selected. Unknown keys are errors with the path of the element (`station[0].role`).

`[[station]]` declares one station:

| Key | Meaning |
| --- | --- |
| `id` | Required. The station id, exactly as it appears in the URL; patterns are not supported. Each id appears once |
| `protocol` | Optional, `"ocpp1.6"` |
| `auth_key_file` | Optional. File holding the Basic Auth key of the station (a trailing line end is dropped). Works like `--auth-key ID=KEY`; entries given with `--auth-key` or `KIRBY_CSMS_AUTH_KEY` win over a block of the same id, and a block wins over the same id in `--auth-keys-file`. Keys need `security_profile` 1 or 2 |

`[[rule]]` answers a CALL that stations send, for all stations or, with `station` and `protocol`, for one station and one protocol. It mirrors the [response rules and faults](csms.md#response-rules-and-fault-injection) of the TUI, the scripts and the HTTP API and is installed before the first station connects:

| Key | Meaning |
| --- | --- |
| `action` | Required. The action name, 1 to 128 bytes without control characters |
| `behavior` | Required. `respond` (answer every CALL of the action with `payload`), or a fault: `call_error`, `silence`, `close`, `invalid_payload` (CALLRESULT with `payload` as given) |
| `payload` | JSON text. Required for `respond` (a JSON object) and `invalid_payload`; refused for the other behaviors |
| `times` | Faults only: occurrences the fault applies to, 1 to 1000000, default 1. Refused for `respond` |
| `station` | Optional. One station id, exactly as in the URL (1 to 128 bytes, no control characters; patterns are not supported). The rule or fault applies to that station only and wins over a rule or fault without `station`. Without it the rule applies to all stations |
| `protocol` | Optional, `"ocpp1.6"`. The rule or fault applies only to stations that negotiated that subprotocol. With `station` and a `respond` rule, `action` must be an action a station of the protocol sends |

Faults are used in the order of the file (a station's faults before global ones) and win over responses; at most 1000 faults are stored, 100 for one station. Rules with `station` or `protocol` count against the same caps as the per-station [rules and faults of the API](api.md#rules-and-faults) (1000 rules, 64 for one station). Resolution order, counts and the effect of `protocol`: [csms.md](csms.md#response-rules-and-fault-injection). A `respond` rule without `station` and `protocol` is the global rule of its action; a later one for the same action replaces it, and a `respond` rule for the same station and action replaces the earlier one.

### `--print-config` and `--check-config`

`--print-config=toml` prints the effective configuration (flags, environment, file and defaults merged, the selected profile applied, no `[profile]` tables, blocks included) as a configuration file. Paths are printed absolute. The output is valid input for `--config`, and printing it again gives the same text. `--print-config` (or `=json`) prints the same content as one JSON object with a table per section and an array per block.

```bash
kirby-csms --config lab.toml --profile ci --print-config=toml > effective.toml
kirby-csms --config effective.toml --print-config=toml | diff - effective.toml
kirby-csms --config lab.toml --profile ci --check-config   # OK: /path/lab.toml (profile ci): configuration is valid
```

`--check-config` runs every check that happens before the server binds, including reading the key files, loading the certificates and the `--script` file, and exits 0 or 2. It does not open the listener or the API port, so it also works while the server runs.

### Example

```toml
schema = 1
protocol = "ocpp1.6"

[listener]
listen = "0.0.0.0:9000"

[api]
api_listen = "127.0.0.1:9100"
api_token_file = "secrets/api.token"    # relative to the directory of this file

[security]
security_profile = 1
default_auth_key_file = "secrets/default.key"

[behavior]
heartbeat_interval = 60

[output]
no_tui = true

[[station]]
id = "CP-001"
auth_key_file = "secrets/cp-001.key"

[[rule]]
action = "Authorize"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Blocked"}}'

[[rule]]
action = "Heartbeat"
behavior = "call_error"
times = 2

[profile.ci.behavior]
heartbeat_interval = 5
```

`kirby-csms --config lab.toml --profile ci` applies the profile; `--heartbeat-interval 10` on the command line wins over both.

## Exit codes

This section is the single reference for exit codes; other pages link here. Code `2` always means a usage or configuration error found before the run starts (bad flag, bad value, invalid scenario or script file). Code `130` is SIGINT; SIGTERM ends a run with `1`.

### kirby-ocpp, plain station

No subcommand: the terminal UI, or headless mode (`--no-tui`, `--json`, no terminal). A fleet (`--count` above 1) follows the same rules.

| Code | Meaning |
| --- | --- |
| 0 | Normal shutdown (quit in the UI, SIGINT or SIGTERM in headless mode). `--print-config` (either format), `--licenses`, `version`, `completions` and `scenarios list|show` also exit 0 |
| 1 | Headless only: the connection was lost for good (`--no-reconnect`, or `--reconnect-max-attempts` used up). The process prints `reconnect stopped (...)` with the last connect error and a hint. A fleet exits 1 when every station gave up. `run` and `suite` also exit 1 with `connection lost: ...` when that happens while a scenario is running. The UI keeps running instead |
| 2 | Usage or configuration error, including a configuration file that cannot be read or breaks a rule (the message names the file and the key), an unknown `--profile` and `--profile` without `--config` |

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
| 0 | Every scenario passed (a scenario that passed on a retry counts as passed and is listed as flaky) |
| 1 | Any scenario failed, timed out or could not boot (also after its retries), or a report file could not be written |
| 2 | Usage error: no source, empty directory, invalid scenario file, `--count` above 1, a selection (`--filter`, `--skip`, `--tag`, `--skip-tag`) that leaves no scenario, `--github-annotations` with `--json`, `--summary-md` naming a scenario or report file |
| 130 | SIGINT. The running scenario ends as an `error` row (`interrupted`), the rest are `skipped`, and `--report` and `--junit` are written for what finished. SIGTERM does the same and exits 1 |

### kirby-ocpp check

| Code | Meaning |
| --- | --- |
| 0 | No step failed (warnings are allowed) |
| 1 | A step failed (the checklist shows which) |
| 2 | Usage or configuration error: no `--url`, an invalid URL, a TLS file that cannot be read, `--count` above 1 |

### kirby-ocpp state

| Code | Meaning |
| --- | --- |
| 0 | `path` printed; `show` printed the state; `reset` deleted the state or found nothing to delete |
| 1 | `show`: no state, an unusable file, or a file of a newer version; `reset`: declined at the prompt, a file of a newer version (nothing is deleted), or a file that cannot be deleted |
| 2 | Usage error: `show` or `reset` without an id, `reset` without `--yes` when stdin is not a terminal, no state directory can be determined |

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
| 2 | Usage or configuration error. Invalid flag combinations and a bad configuration file or profile are found before the server binds |

`--check-config` exits 0 after printing one `OK:` line, or 2 with the error; `--print-config` exits 0 with the settings, or 2 on an invalid flag combination.

### kirby-csms --script

| Code | Meaning |
| --- | --- |
| 0 | The script passed |
| 1 | A step failed or timed out (`--timeout`), the server could not start, or a report file could not be written. On SIGTERM the `--junit` file still gets one failed suite (`interrupted`) |
| 2 | Usage, configuration or script error |
| 130 | SIGINT, no report |

## Related pages

[Emulator](emulator.md) · [CSMS](csms.md) · [Scenarios](scenarios.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
