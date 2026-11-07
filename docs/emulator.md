# Emulator (`kirby-ocpp`)

`kirby-ocpp` is a terminal OCPP 1.6J charge point (station). It connects to a central system (CSMS), answers its commands, and lets you drive the station by hand (terminal UI), log it (headless) or script it ([scenarios](scenarios.md)). The Security Whitepaper messages are supported too.

Contents: [Quick start](#quick-start) · [Modes](#modes) · [Terminal UI](#terminal-ui) · [Headless mode and exit codes](#headless-mode-and-exit-codes) · [Connection errors](#connection-errors) · [Output levels and color](#output-levels-and-color) · [Version, completions and validation](#version-completions-and-validation) · [Suite mode](#suite-mode) · [Fleet mode](#fleet-mode) · [Response rules](#response-rules) · [State persistence](#state-persistence) · [TLS](#tls) · [Related pages](#related-pages)

## Quick start

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001           # TUI (stdout is a terminal)
kirby-ocpp --url ws://localhost:9000/ocpp --no-tui --json       # headless, JSON lines
kirby-ocpp --url ws://localhost:9000/ocpp run --scenario charge-session
```

`--url` (or `OCPP_URL`) is the only required option (without it the error shows this example and suggests `kirby-csms --listen 127.0.0.1:9000` as a local peer); the station id defaults to `CP-001`. Precedence is flag, then environment variable, then default. Booleans accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive; anything else is a usage error (exit 2). The full flag and environment table is in [configuration.md](configuration.md); `kirby-ocpp --help` always matches the binary. `--print-config` prints the effective configuration as JSON and exits (URL credentials are redacted); `--print-config=toml` prints it as a configuration file. Settings can also come from a TOML file (`--config`, optionally `--profile`): see [Configuration file](#configuration-file).

The station connects at startup in every mode (`--print-config`, `version`, `completions` and `scenarios` exit before connecting). If the first connect fails, the error is printed and the reconnect loop keeps trying (backoff `--reconnect-initial-ms` to `--reconnect-max-ms`). In headless mode, `run` and `suite` the process exits with code 1 once the loop ends for good: with `--no-reconnect`, or when `--reconnect-max-attempts` is used up. See [Connection errors](#connection-errors).

## Modes

| Invocation | Result |
| --- | --- |
| no subcommand, stdout is a terminal, no `--json` or `--no-tui` | [Terminal UI](#terminal-ui) |
| no subcommand, `--no-tui`, `--json`, or stdout is not a terminal | [Headless](#headless-mode-and-exit-codes): connect, print events until SIGINT/SIGTERM |
| `run --scenario NAME_OR_FILE` | Connect, run one [scenario](scenarios.md), print the report, exit with its verdict. Ephemeral: no state file unless `--persist` |
| `suite SOURCE...` | Run several scenarios, each on a fresh station, see [Suite mode](#suite-mode) |
| `scenarios list` / `show NAME` / `validate FILE\|DIR\|NAME...` | Built-in scenarios and scenario checks, no network |
| `version`, `completions SHELL` | Build details and shell completion scripts, no network |
| `--count N` (above 1) | [Fleet](#fleet-mode) of N stations, with any of the above |

`--json` is for machines: it never opens the UI. If the terminal cannot be set up (no usable TTY), the emulator prints `note: ...; running headless` and continues headless. `--log-file PATH` appends every event to a file (also under the UI, where stdout stays free for the screen).

## Terminal UI

Seven tabs, switched with `1`-`7` or `Tab`/`Shift-Tab`: Station, Connector, Transaction, Maintenance, Data, Logs, Scenarios. Three panes: connectors, actions, log; `Left`/`Right` moves the focus.

| Key | Action |
| --- | --- |
| `Up/Down`, `j/k`, `PageUp/PageDown` | Move in the focused pane |
| `[` `]` | Previous / next connector |
| `Enter` | Run the selected action / open the selected log row |
| letters | Accelerator of an action of the current tab (shown in the list) |
| `/`, `:` or `Ctrl-P` | Command palette (fuzzy search over all actions); `/` searches the log when the log pane is focused on the Logs tab |
| `Esc` | Close a dialog; in the main view, clear the log search |
| `?` | Help (`Esc`, `?`, `q` or `Enter` closes it) |
| `q`, `Ctrl-C` | Quit. Asks first (default No) while a scenario is running, a connector has an active transaction, or the fleet has more than one station; a second `Ctrl-C` while the question is open quits at once. In a form, the data browser or another view, `Ctrl-C` asks the same way and a "no" brings the view back; during another confirmation it quits at once |

Confirmations (quit, clear log, clear list, ...) accept only `y`/`Y`; any other key cancels. The quit question also accepts `q`.

The footer always ends with the `? help` and `: palette` hints, even in a narrow terminal; other hints are dropped first. While a command that waits for the CSMS is in flight (from a key, the palette or a form), the footer shows `waiting... <action>` until the result arrives; a notice takes that line while it is shown, and the marker is dropped when the connection goes down.

First steps: the station connects and boots at startup, and the header shows the connection state. On the Station tab `x` disconnects and `c` connects again (pressing `c` while connected says so). Press `:` and type an action name (BootNotification, Heartbeat, StartTransaction) to run it, or open the Scenarios tab (`7`). Every frame lands in the log. The help popup (`?`) opens with the same hints; it wraps to the terminal width.

The UI needs at least 24 columns by 8 rows (9 rows for a fleet, which adds the selector row; the CSMS UI needs 24 by 8); a smaller terminal shows `need 24x8, have WxH` until it is resized. Forms and popups use the full width of a narrow terminal, and long station ids are shortened in the middle so the connection state stays visible. The value of an `AuthorizationKey` is drawn as `*` while you type it (paste and editing work as usual). The log filter `errors` shows ERROR frames and error-level lines only.

### Actions by tab

| Tab | Actions (accelerator) |
| --- | --- |
| Station | BootNotification `b`, Heartbeat `h`, Reconnect `r`, Disconnect `x`, Connect `c`, add connector `a`, response rule `m`, clear response rules `M`, config keys `K`, local auth list `L`, charging profiles `P`, certificates `Y`, request certificate `C`, security event `S` |
| Connector | plug / unplug `p`, cycle status `s`, set status `t`, StatusNotification `n`, fault / clear fault `f` |
| Transaction | Authorize `a`, StartTransaction `t`, StopTransaction with reason `e`, MeterValues +120 Wh `m`, MeterValues custom `c` |
| Maintenance | diagnostics outcome `d`, firmware outcome `u`, DiagnosticsStatusNotification `i`, FirmwareStatusNotification `f` |
| Data | DataTransfer `d`, vendor ping `v`, raw CALL `r` |
| Logs | see [Log view](#log-view) |
| Scenarios | see [Scenarios tab](#scenarios-tab) |

Keys `q j k ? / : [ ] 1`-`7` are never accelerators. The Station pane shows the active response rules, the security profile, the certificate count, queued security events, a pending CSR, and the last log-upload and signed-firmware status with their request ids.

Security Whitepaper actions: `C` creates a key pair and a CSR and sends `SignCertificate`; `S` reports a security event (pick one of the 21 whitepaper event types, optional `techInfo`); `Y` opens the certificate browser.

### Data browser

`K` (config keys), `L` (local authorization list), `P` (charging profiles) and `Y` (certificates) open one popup with four pages, switched with `Tab`/`Shift-Tab`, `Left`/`Right`, `h`/`l` or `1`-`4`. `/` filters the rows fuzzily (`Enter` keeps the filter, `Esc` clears it). `Up/Down`, `j/k`, `PageUp/PageDown`, `g`/`G` move. `Esc` or `q` clears an active filter first, then closes. Forms and confirmations opened from the browser return to it.

Form fields take at most 256 KiB (typed or pasted; the rest is dropped and the field says so). DataTransfer limits `vendorId` to 255 characters, `messageId` to 50 and `data` to 64 KiB. Ids and numbers are trimmed; a configuration value and DataTransfer `data` keep their leading and trailing spaces. The log export (`e`) is written with mode 0600 (an existing file is tightened first) and is refused when the path is a symlink.

Edits are local commands that behave like the CSMS-triggered ones, without a CSMS round trip:

| Page | Keys |
| --- | --- |
| Config keys | `Enter`/`e` edits the selected key (read-only keys are marked `(ro)` and refuse). The value is checked live with the station's own rules, as `ChangeConfiguration` would, and applied with the same side effects (heartbeat and metering timers) and persistence |
| Local auth list | `a` adds, `Enter`/`e` edits (idTag, status, expiry, parent), `d`/`Delete` removes, `c` clears (asks first). Each add, edit or remove is a differential update that bumps the list version by one; clear empties the list and resets the version to 0 |
| Charging profiles | `i` installs a profile from JSON (a template is prefilled; paste your own: a `csChargingProfiles` object plus connector, validated against the `SetChargingProfile` schema and the station's profile rules), `e` edits the selected one (same id replaces it), `Enter` shows its JSON (`c` there copies), `d` deletes it, `c` clears all (asks first) |
| Certificates | Read-only: type, subject, expiry. Install and delete come from the CSMS |

The data behind the pages (`Snapshot::config`, `local_list`, `charging_profiles`) is also available to API users; the local list view holds at most 500 entries (`total` has the real count).

### Log view

Logs tab. Keys:

| Key | Action |
| --- | --- |
| `f` | Cycle the filter: all, calls, errors, station, csms |
| `s` or `/` | Search |
| `p` | Pause / follow |
| `G` | Jump to the newest row |
| `Enter` | Detail view with scroll (`c` or `y` copies it) |
| `y` / `Y` | Copy the selected row / the visible rows |
| `e` | Export as JSONL, default `./kirby-ocpp-<id>-<timestamp>.jsonl` |
| `c` | Clear (asks first) |

CALL, RESULT and ERROR frames are correlated. The buffer keeps the last 2000 entries and at most about 64 MiB (fleet: less per station). A frame or line longer than 16 KiB is stored cut, ending in `...[truncated N bytes]`; copy and the detail view then show the cut text, JSONL export writes it as a string, and search looks at the first 4 KiB only. Copying tries external clipboard tools first (a tool still running after 500 ms is killed) and falls back to the terminal's OSC 52 sequence (up to about 55 kB of text).

### Scenarios tab

One `Run: <name>` action per built-in scenario (keys `o`, `w`, `s`, `i` for the first four, in the order of `scenarios list`; the others are reached with `Enter` or the command palette), `l` to load a scenario file, `x` to cancel the running one. Only one scenario runs per station at a time. The scenario format is in [scenarios.md](scenarios.md).

## Headless mode and exit codes

Without a TTY, or with `--no-tui` or `--json`, the emulator connects and prints events until SIGINT or SIGTERM, then closes the WebSocket cleanly and exits 0. Human lines look like `12:00:01.234 -> [2,"...","Heartbeat",{}]` (`->` sent, `<-` received) and `12:00:01.300 [Info] text`.
- With `--json` each line is one JSON object with a `type` of `frame` (carries a `payload` field next to the raw `frame`), `log` or `state_changed`. If the stdout reader is too slow, events are dropped and a `note` line says how many.
- Text that comes from the CSMS is sanitized (control characters replaced or escaped) before it is printed.

If the station gives up reconnecting (`--no-reconnect`, or `--reconnect-max-attempts` used up), the headless process no longer idles: it closes cleanly, prints `error: reconnect stopped (REASON); last error: ...` to stderr and exits 1. The give-up is read from the station snapshot field `reconnect_stopped` (the reason text, `null` otherwise; cleared by the next connect), so a lost event stream cannot hide it. A fleet exits 1 when every station has given up. The terminal UI keeps running (it shows the state itself).

`kirby-ocpp run --scenario NAME_OR_FILE` connects, waits for an accepted BootNotification, runs the scenario and prints a PASS/FAIL/SKIP report (a JSON object with `"type": "report"` under `--json`). Frame and log lines are not printed unless you pass `-v` (see [Output levels and color](#output-levels-and-color)). A `run` does not read or write a state file, so it is repeatable; `--persist` keeps state like a plain station. A connection that cannot be established (or is lost for good) ends the run at once with exit 1 and the same message as in headless mode; with reconnect enabled the run waits until `--timeout` and the timeout message names the last connection error. The run options (`--report`, `--junit`, `--timeout`, `--id-tag`), the report shapes and CI use are in [scenarios.md](scenarios.md#running-scenarios).

| Code | Meaning |
| --- | --- |
| 0 | Success (scenario passed, normal shutdown) |
| 1 | Scenario failed or timed out, a runtime error, the connection was lost for good (reconnect stopped), or a report file could not be written |
| 2 | Usage or configuration error (including a bad scenario file, an unknown scenario name, `--persist` with `--no-persist`) |
| 130 | `run` or `suite` interrupted by SIGINT (Ctrl-C); SIGTERM still ends it with 1. `suite` writes `--report` and `--junit` for the finished scenarios on both signals (the interrupted one is an `error` row, the rest `skipped`) |

## Connection errors

A failed connection prints the error plus one line of advice for its class:

| Error | Hint |
| --- | --- |
| connection refused | check the host and port and that the CSMS is running |
| TLS handshake EOF or garbage against a `wss://` URL | the server may speak plain ws:// |
| unknown certificate authority | use `--tls-ca` or `--tls-skip-verify` |
| HTTP 401 or 403 on the upgrade | check the credentials (`AuthorizationKey`, URL `user:password`) and the security profile |
| HTTP 400 or 404 on the upgrade, or no subprotocol agreed | the status and the offered subprotocol (from `--ws-subprotocol`) are repeated with: check the URL path and `--ws-subprotocol` |
| TLS alert `CertificateRequired` or `BadCertificate` | the server wants a client certificate: check `--tls-cert` and `--tls-key` |
| certificate `NotValidForName` | the certificate does not match the host name: use `--tls-server-name` |

The text is `error. Hint: ...` on one line, in the stderr message, the `suite` row and the timeout message of `run`. The log events keep the plain error text.

## Output levels and color

`run` and `suite` print the report only. `-v, --verbose` adds the frame and log lines as they happen (what plain headless mode always prints); `-q, --quiet` leaves only errors (stderr) and the final verdict line. `--json` keeps printing events as before. The exact output of each level is in the [configuration reference](configuration.md#output-levels). `--log-file` always gets the full event log.

Human output (not `--json`, not the terminal UI) colors PASS green, FAIL red and SKIP and warnings yellow when `--color auto` (the default) finds a terminal and no `NO_COLOR` variable (set and not empty). `--color always` and `--color never` decide explicitly, also over `NO_COLOR`. Text from the CSMS is sanitized before it is colored, so it cannot add escape sequences.

## Version, completions and validation

* `kirby-ocpp --version` prints one line. `kirby-ocpp version` prints the version, git commit, build date, target triple and supported protocols (`OCPP 1.6J`); `kirby-ocpp version --json` prints them as one JSON object.
* `kirby-ocpp scenarios validate FILE|DIR|NAME...` parses scenarios without connecting and without `--url`: `ok PATH (N steps)` per valid scenario, the load error with the path otherwise, exit 0 or 2. A directory is scanned like `suite` does. A file named twice is checked once, a scenario name used by two files gives a `warning:` line (exit stays 0), and `*.toml` entries that are not regular files (dangling symlinks) are skipped with a `note:` line.

### Shell completions

`kirby-ocpp completions <bash|zsh|fish|powershell|elvish>` prints the script to stdout. Install it once:

```bash
# bash (a user-level completion directory; or source it from ~/.bashrc)
kirby-ocpp completions bash > ~/.local/share/bash-completion/completions/kirby-ocpp
# zsh (any directory in $fpath, before compinit)
kirby-ocpp completions zsh > ~/.zfunc/_kirby-ocpp
# fish
kirby-ocpp completions fish > ~/.config/fish/completions/kirby-ocpp.fish
```

Restart the shell afterwards. PowerShell: `kirby-ocpp completions powershell | Out-String | Invoke-Expression` in the profile.

## Suite mode

`kirby-ocpp suite [SOURCE...]` runs several scenarios one after another against the same CSMS, for conformance-style runs and CI. A SOURCE is a scenario file, a directory (every `*.toml` directly inside, sorted by name, not recursive) or a built-in name; `--all-builtins` adds every built-in except `finish-session` (it needs a transaction left by an earlier run, which a fresh station never has; name it explicitly to run it anyway).

```bash
kirby-ocpp --url ws://localhost:9000/ocpp suite scenarios/ charge-session --junit junit.xml --report suite.json
```

Every scenario runs on its own new station (same URL, id and station options as `run`), never in parallel. A scenario whose station cannot connect (or gives up reconnecting) fails at once with the connection error. Persistence is switched off for suite runs (`--state-dir` and `--no-persist` are ignored), so no state carries over from one scenario to the next and nothing is written to disk. Human output is one line per scenario, then a summary:

```text
PASS charge-session (1.2 s)
FAIL my-auth: authorize - Blocked
SKIP last-one
suite: 1/3 scenarios passed, FAILED: my-auth, SKIPPED: last-one
```

`--fail-fast` skips the rest after the first failure. `--count` above 1 is a usage error (exit 2). Options, JSON and JUnit shapes: [scenarios.md](scenarios.md#suite-runs); exit codes: [configuration.md](configuration.md#kirby-ocpp-suite).

## Fleet mode

`--count N` (1 to 1000) runs N independent stations in one process against the same CSMS, for load and multi-station tests. The UI shows them on a TTY; otherwise they run headless.

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n:03}' --count 50 --stagger-ms 100 --no-tui
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n}' --count 20 run --scenario charge-session
```

* **Ids.** The id comes from `--id-template`, or from `--id` when no template is given. `{n}` is the station index, `{n:03}` the index zero-padded to 3 digits; `{{` and `}}` are literal braces. With `--count` above 1 the template needs a placeholder (otherwise exit 2). `--first-index` (default 1) is the index of the first station: `--count 3 --first-index 10` gives `CP-10`, `CP-11`, `CP-12`. `--count 1` without `--id-template` uses `--id` as is; with an explicit template it is expanded too.
* **Stations.** Every station is a normal station: own connection, transactions, and state file named after its id, so state files never collide. `--stagger-ms` waits that long between starting two stations.
* **Output.** Every human line is tagged `[CP-001] ...` and every JSON line has a `"station"` field, on stdout and in `--log-file`. With `--count 1` nothing is tagged.
* **`run --scenario`.** The scenario runs on every station at the same time and prints one report per station (tagged), then one aggregate line. `--timeout` covers the whole fleet, stagger included. Report files: see [scenarios.md](scenarios.md#fleet-runs).
* **`--print-config`** with `--count` above 1 prints `{"fleet": {...}, "config": {...}}`, where `config` is the first station.

The UI shows a selector row on top (`1 CP-001+* 2 CP-002- ...`: index, id, connection glyph `+` connected, `~` connecting or retrying, `-` offline, `*` a transaction is running) and the selected station below. Everything else (status, log, actions, browsers, forms, scenarios) works on the selected station only. Each station keeps its own log, dialogs and scenario progress, and events of the other stations are still recorded. The log keeps 500 rows per station for up to 40 stations, then fewer (minimum 50). Quit stops the whole fleet. With `--count 1` there is no selector and no extra keys.

| Fleet key | Action |
| --- | --- |
| `<` `>` | Previous / next station |
| `Alt-1` .. `Alt-9` | Jump to station 1..9 |
| `Alt-c` / `Alt-d` | Connect / disconnect all stations |

Selector keys work in normal mode only (inside a dialog they are plain input). `Alt` combinations need "Option as Meta" in macOS Terminal.

Exit codes in fleet mode are the same as for a single station: see [configuration.md](configuration.md#exit-codes).

## Configuration file

`--config FILE` (or `KIRBY_OCPP_CONFIG`) reads a TOML file with the same settings as the flags, and `--profile NAME` (or `KIRBY_OCPP_PROFILE`) applies one of its `[profile.NAME]` variants. Flags and environment variables win over the file; the file wins over the defaults.

```toml
[connection]
url = "ws://localhost:9000/ocpp"

[fleet]
count = 5
id_template = "CP-{n:03}"

[profile.staging.connection]
url = "wss://staging.example/ocpp"
```

```bash
kirby-ocpp --config lab.toml --no-tui                      # five stations against the local CSMS
kirby-ocpp --config lab.toml --profile staging --count 1   # one station, staging URL
kirby-ocpp --config lab.toml --print-config=toml           # the merged result, as a file
```

Relative paths in the file resolve against the directory of the file. A password never goes into the file; unknown keys are errors. Format, tables, key names and profiles: [configuration.md](configuration.md#configuration-file).

## Response rules

Response rules make the station misbehave on purpose, to test how a CSMS reacts. A rule overrides how one CSMS command is answered. Any of the 26 commands the station handles can be targeted: the 19 of OCPP 1.6 and the 7 Security Whitepaper ones (for example `Reset`, `RemoteStartTransaction`, `InstallCertificate`, `GetLog`, `ExtendedTriggerMessage`, `SignedUpdateFirmware`). Form: `ACTION=BEHAVIOR[*COUNT]`, action names case-insensitive.

| Behavior | Effect |
| --- | --- |
| `reject` | The action's own rejection: `Rejected`; `NotSupported` for `SendLocalList` and `UnlockConnector`; `Failed` for `DeleteCertificate`; `NotFound` for `GetInstalledCertificateIds`. `GetConfiguration`, `GetLocalListVersion`, `ClearChargingProfile`, `GetDiagnostics` and `UpdateFirmware` have no rejection status, so they get CALLERROR `NotSupported` |
| `delay:MS` | The command is handled normally; only the reply is sent `MS` ms later (0 to 3600000). The station keeps working meanwhile |
| `error:CODE[:TEXT]` | CALLERROR with an OCPP-J error code (for example `NotImplemented`, `NotSupported`, `InternalError`, `ProtocolError`, `SecurityError`, `FormationViolation`, `PropertyConstraintViolation`, `OccurenceConstraintViolation`, `TypeConstraintViolation`, `GenericError`) |
| `drop` | Close the WebSocket without replying; the normal reconnect lifecycle follows (disabled by `--no-reconnect`) |
| `ignore` | Never reply, so the CSMS call timeout fires |

`*COUNT` applies the rule that many times and then removes it (default: unlimited). There is one rule per action; setting another replaces it. Except for `delay`, the command is not executed (no side effects). Rules are runtime only and never persisted. Each application is logged (`response rule: Reset -> reject, 2 more`).

Ways to set rules:

* `--rule` (repeatable) or `KIRBY_OCPP_RULES`. In both, `;` separates several rules, so a rule (an `error:` text, say) must not contain `;`.
* The TUI, Station tab: `m` adds one, `Action=off` removes one, `M` clears all.
* Scenario steps `set_rule` and `clear_rules` ([scenarios.md](scenarios.md#actions)).
* `Command::SetResponseRule` / `ClearResponseRules` in the core API.

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --rule Reset=reject --rule GetConfiguration=delay:5000 \
  --rule ChangeConfiguration=error:InternalError*3 --rule RemoteStartTransaction=ignore*1
```

## State persistence

Per charge point id, one JSON file `<state dir>/<id>.station.json` holds everything that must survive a restart:

* the local authorization list and its version, and the authorization cache;
* charging profiles and writable configuration values;
* availability (including a `Scheduled` change waiting for a transaction to end), connector count, and per-connector state (status, meter, reservation, transaction);
* the offline transaction queue and the queue of `MeterValues` samples taken during a running offline transaction (at most 1000, delivered in order after reconnecting);
* a pending security profile switch (the previous profile and a failure count, used by the A05.FR.08 fallback; see [security.md](security.md));
* installed certificates (public data) and queued security events.

Secrets (such as private keys) live in a separate `<id>.station.secrets.json`.

The state directory is `--state-dir` (`KIRBY_OCPP_STATE_DIR`), else `~/.kirby-ocpp/stations` (`%USERPROFILE%` on Windows). Special characters in the id are percent-escaped in the file name; an id whose escaped name would exceed 100 bytes is shortened and gets a hash suffix (`...~<16 hex>.station.json`) so the name always fits the filesystem limit.

Files are `0600` on Unix, versioned, and written atomically (temp file plus rename, then the directory is fsynced). A file with a newer version than the binary supports is an error and is left alone. A corrupt file, or one that belongs to another charge point, is moved to `<file>.bak` and the station starts clean. `--no-persist` disables both loading and saving. Response rules are never saved.

## TLS

`wss://` uses rustls. Without `--tls-ca` the operating system trust store is used. Use `--tls-ca` for a private CA, `--tls-cert` and `--tls-key` for client authentication (mutual TLS), `--tls-server-name` when the URL host differs from the certificate name, and `--tls-skip-verify` only against local self-signed servers.

```bash
kirby-ocpp --url wss://csms.example/ocpp --tls-ca ca.pem --tls-cert cp.pem --tls-key cp.key
```

Frames that carry `AuthorizationKey` are logged with the value replaced by `***` unless you pass `--show-secrets`. Security profiles 1 to 3, Basic Auth, certificate signing and the station's certificate store are described in [security.md](security.md).

## Related pages

* [scenarios.md](scenarios.md): scenario files, `run`, reports, fleet runs.
* [configuration.md](configuration.md): every flag and environment variable.
* [security.md](security.md): security profiles, certificates, mutual TLS.
* [architecture.md](architecture.md): crate map, `StationHandle` contract, invariants.
* [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md): protocol coverage and deviations.
