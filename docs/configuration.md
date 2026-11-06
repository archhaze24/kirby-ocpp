# Configuration reference

Every command-line flag and environment variable of `kirby-ocpp` and `kirby-csms`, with defaults, ranges, exit codes and the files they touch. The tables follow `--help` of the same version; if they ever disagree, `--help` and `cli.rs` are right and this page is a bug (see [Development](development.md#keeping-docs-in-step-with-code)).

Contents: [Precedence and booleans](#precedence-and-booleans) · [kirby-ocpp](#kirby-ocpp) · [Subcommands](#kirby-ocpp-subcommands) · [Id templates](#id-templates) · [Response rules](#response-rules---rule) · [State directory](#state-directory-and-files) · [kirby-csms](#kirby-csms) · [Exit codes](#exit-codes) · [Related pages](#related-pages)

## Precedence and booleans

* Order: command-line flag, then environment variable, then built-in default. Both binaries use `clap` with `env =`.
* Boolean values (env vars and flags that take `<BOOL>`) accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive. Anything else is a usage error (exit 2). An empty variable counts as unset.
* Pure switches (`--no-persist`, `--tls-skip-verify`, `--show-secrets`, `--json`, `--no-tui`, ...) are set by the flag or by a true value in their variable.
* Numeric options reject out-of-range values with exit 2 (ranges below). Text options reject empty values.

## kirby-ocpp

`kirby-ocpp [OPTIONS] [COMMAND]`. Options marked global are also accepted after the subcommand (`run`, `scenarios`). `--url` (or `OCPP_URL`) is the only required value for a real connection.

### Station and connection

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `-u, --url <URL>` | `OCPP_URL` | none (required) | Central System WebSocket URL, `ws://` or `wss://`, e.g. `ws://localhost:9000/ocpp` |
| `-i, --id <ID>` | `OCPP_CHARGE_POINT_ID` | `CP-001` | Charge point id (non-empty); with `--count` above 1 it is the id template unless `--id-template` is given |
| `--vendor <VENDOR>` | `OCPP_VENDOR` | `archhaze24` | BootNotification vendor (non-empty) |
| `--model <MODEL>` | `OCPP_MODEL` | `KIRBY` | BootNotification model (non-empty) |
| `--serial <SERIAL>` | `OCPP_SERIAL` | none | Serial number in BootNotification, BootNotification schema limit: 25 characters |
| `--firmware-version <VERSION>` | `OCPP_FIRMWARE_VERSION` | none | Firmware version in BootNotification, BootNotification schema limit: 50 characters |
| `-c, --connector <ID>` | | `1` | Initial connector id (at least 1) |
| `--connectors <COUNT>` | | `1` | Number of connectors (at least 1; raised to `--connector` if smaller) |
| `--heartbeat <SECONDS>` | | `30` | Fallback heartbeat interval (at least 1); the CSMS value from BootNotification wins |
| `--nominal-power-w <WATTS>` | | `7200` | Nominal charging power per connector (at least 1); charging profiles can only lower it |
| `--voltage-v <VOLTS>` | | `230` | Voltage per phase (at least 1), used for A to W conversion and the `Voltage` reading |
| `--phases <COUNT>` | | `1` | Number of phases, 1 to 3 |
| `--ws-subprotocol <PROTOCOL>` | `OCPP_WS_SUBPROTOCOL` | `ocpp1.6` | Requested WebSocket subprotocol |
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
| `--tls-ca <PATH>` | `OCPP_TLS_CA_FILE` | system roots | CA bundle (PEM) for `wss://` |
| `--tls-cert <PATH>` | `OCPP_TLS_CERT_FILE` | none | Client certificate for mutual TLS; needs `--tls-key` |
| `--tls-key <PATH>` | `OCPP_TLS_KEY_FILE` | none | Client private key for mutual TLS; needs `--tls-cert` |
| `--tls-server-name <NAME>` | `OCPP_TLS_SERVER_NAME` | URL host | SNI and verification name override |
| `--tls-skip-verify` | `OCPP_TLS_SKIP_VERIFY` | off | Disable certificate verification (local or self-signed testing only) |
| `--show-secrets` | `OCPP_SHOW_SECRETS` | off | Show `AuthorizationKey` values in frame logs; by default they are replaced by `***` |
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
| `--no-persist` | `KIRBY_OCPP_NO_PERSIST` | off | Do not load or save station state |
| `--state-dir <PATH>` | `KIRBY_OCPP_STATE_DIR` | `~/.kirby-ocpp/stations` (`%USERPROFILE%` on Windows) | Directory for persisted state, see [State directory](#state-directory-and-files) |
| `--rule <ACTION=BEHAVIOR>` | `KIRBY_OCPP_RULES` | none | Response rule, repeatable; in the variable separate rules with `;`. See [Response rules](#response-rules---rule) |

### Output

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--print-config` | | | Print the effective configuration as JSON and exit. With `--count` above 1 it prints `{"fleet": {...}, "config": {...}}`, `config` being the first station |
| `--no-tui` | | | Never start the terminal UI; log to stdout (automatic when stdout is not a terminal) |
| `--json` | | | One JSON object per line (events and the `run` report); never opens the UI |
| `--log-file <PATH>` | | none | Also append the event log to this file, with the UI too. A new file is created `0600` on unix (frames hold idTags); an existing file keeps its mode. A helper thread writes it; lines that cannot be written in time are dropped and a `note` line reports the count |
| `--licenses` | | | Print third-party licence notices and exit |
| `-V, --version`, `-h, --help` | | | |

## kirby-ocpp subcommands

`kirby-ocpp run --scenario <FILE|NAME>` connects, waits for an accepted BootNotification, runs the scenario and prints the report. It accepts every option above plus:

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--scenario <FILE\|NAME>` | | required | Built-in scenario name or a TOML file |
| `--report <PATH>` | | none | Write the report as JSON. In fleet mode one file per station: `out.CP-001.json` (characters of the id other than letters, digits, `-`, `_` become `_`) |
| `--junit <PATH>` | | none | Write a JUnit XML report; one file, one test suite per station in a fleet |
| `--timeout <SECONDS>` | | `60` | Give up and fail after this long, connecting included; 1 to 86400. In fleet mode it covers the whole fleet, stagger included |
| `--id-tag <IDTAG>` | | none | Replace the `idTag` of the scenario's `authorize`, `start_tx`, `stop_tx` steps (non-empty) |

`kirby-ocpp scenarios list` lists the built-in scenarios; `kirby-ocpp scenarios show <NAME>` prints one as TOML. Scenario format: [Scenarios](scenarios.md).

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

```
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
| `--tls-cert <FILE>` | `KIRBY_CSMS_TLS_CERT` | none | Server certificate chain (PEM) for `wss://`; needs `--tls-key` |
| `--tls-key <FILE>` | `KIRBY_CSMS_TLS_KEY` | none | Server private key (PEM); needs `--tls-cert` |
| `--tls-client-ca <FILE>` | `KIRBY_CSMS_TLS_CLIENT_CA` | none | Trusted CA(s) for client certificates; with `--tls-cert` every client must present one (mutual TLS); needs `--tls-cert` |
| `--security-profile <0..3>` | `KIRBY_CSMS_SECURITY_PROFILE` | `0` | `0` none, `1` Basic Auth, `2` Basic Auth plus TLS, `3` TLS with client certificates (CN must equal the station id) |
| `--auth-key <ID=KEY>` | `KIRBY_CSMS_AUTH_KEY` (`ID=KEY;ID=KEY`) | none | Basic Auth key of one station (repeatable). The environment variable is used only when no `--auth-key` is given. The flag value is visible in `ps`; prefer the variable or `--auth-keys-file` |
| `--auth-keys-file <FILE>` | `KIRBY_CSMS_AUTH_KEYS_FILE` | none | Lines `ID=KEY`; blank lines and `#` comments are skipped. Keep the file private (permissions are not checked) |
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
| `--timeout <SECONDS>` | | `60` | Fail the script after this long, 1 to 86400; needs `--script` |
| `--licenses` | | | Print third-party licence notices and exit |
| `-V, --version`, `-h, --help` | | | |

CSMS usage and the script format: [CSMS](csms.md), [Scenarios](scenarios.md).

## Exit codes

| Code | `kirby-ocpp` | `kirby-csms` |
| --- | --- | --- |
| 0 | Success (scenario passed, normal shutdown) | Success or clean shutdown |
| 1 | Scenario step failed or timed out, or runtime error; in fleet mode, any station failed | Script failed or timed out, or runtime error |
| 2 | Usage or configuration error | Usage or configuration error |
| 130 | `run` interrupted by SIGINT (SIGTERM still ends with 1) | Script interrupted by SIGINT |

## Related pages

[Emulator](emulator.md) · [CSMS](csms.md) · [Scenarios](scenarios.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
