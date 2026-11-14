# HTTP control API

Drive a running `kirby-csms` from any language or CI without stdin plumbing. JSON, plain HTTP or HTTPS, off by default.

Contents: [Contract](#contract) · [Start](#start) · [Auth model](#auth-model) · [TLS](#tls) · [Endpoints](#endpoints) · [Bulk calls](#bulk-calls) · [Event stream](#event-stream) · [History, export and metrics](#history-export-and-metrics) · [Rules and faults](#rules-and-faults) · [Command-line client](#command-line-client) · [Errors and limits](#errors-and-limits) · [Related pages](#related-pages)

## Contract

The machine-readable [OpenAPI document](kirby-csms-openapi.json) lists the routes, parameters,
request bodies and response formats. It ships as `kirby-csms-openapi.json` in every release
archive and as a separate release asset. A test checks its paths and methods against the
server's route table. Protocol-defined payloads and station state remain protocol-dependent.
The document describes explicit routes; `HEAD` for `GET` routes and `OPTIONS` follow the
common behavior below.

The shapes in this page are a public contract, versioned by the path prefix and by `api_version` (`GET /v1` and `GET /v1/health` return it; now `1`).
- Stable within `/v1`: the paths and methods in the table below, the field names and types of every JSON body, the error `code` values, the SSE event types and their `id:` lines. Additive changes (new fields, new event types, new endpoints, new error codes) keep `/v1`: clients must ignore fields and event types they do not know. A change that removes or renames a field, changes a type, or changes the meaning of a status or code is breaking and ships as `/v2` (with `/v1` kept for a release where feasible).
- Not part of the contract: the text of `message` fields and of `error` strings in call answers, the order of object keys, the order of stations in a list, and the `ts` precision.
- The API is protocol-neutral. A station object carries `protocol`, the WebSocket subprotocol negotiated for its connection (`"ocpp1.6"` today, `null` when the subprotocol policy selected none), so that stations of other OCPP versions can be served on the same listener without a new API version. Protocol ids are strings in the API (see [Ids](#ids)).
- Protocol-level outcomes of a call (the station answered with a CALLERROR, or did not answer in time) are HTTP `200` with `"ok":false`; HTTP errors mean the request was not delivered or was not valid, see [Errors and limits](#errors-and-limits).
- Browsers are refused by design: there are no CORS headers, and a request with an `Origin` header is refused without a token. `OPTIONS` answers `204` with an `Allow` header for every known path (behind the same token and `Host` checks) and is meant for tools, not for browser preflights.
- `Allow` is derived from the route table that `GET /v1` lists and appears only on a `405` and on the `204` answer to `OPTIONS`, in one format: the route's methods plus `HEAD` (for a `GET` route) and `OPTIONS`, sorted, joined by a comma and a space (`GET, HEAD, OPTIONS`). No other status (`200`, `401`, `403`, `404`, ...) carries it.
- Absent and `null`: a field that exists in a shape is always present, with one exception: the outcome-dependent fields of the single-call answer (`POST /v1/stations/{id}/calls`). `outcome` and `ok` are always present; `result` is present only for `outcome` `result`; `error`, `code` and `details` are present only for `call_error`; a `timeout` carries `error` and no `result`, `code` or `details`. A value that is not known yet is an explicit `null` (a station that never booted has `"vendor":null`), a collection that is empty is `[]` or `{}`, a counter is `0`. A key that is missing means the field does not exist in this version of the API: clients must tolerate keys they do not know and must not rely on a key being absent. Optional extension fields that are not defined yet are simply not sent.
- Golden shapes (top-level keys, checked by the test suite): health `ok`, `api_version`, `version`, `stations`; index `api_version`, `version`, `resources` (each `path`, `methods`); station object (in the list and in `GET /v1/stations/{id}`) `id`, `connected`, `protocol`, `received_calls`, `remote_addr`, `vendor`, `model`, `connectors`, `active_transactions`, `security_events`, and `state` in `GET /v1/stations/{id}` only; history `station`, `frames`, `next_cursor`, `gap`, each history frame the keys of an event frame plus `cursor`; transaction list `transactions`, `truncated`, each transaction `station`, `id`, `connector`, `id_tag`, `id_tag_status`, `meter_start`, `started_at`, `status`, `meter_stop`, `stopped_at`, `stop_reason`, `energy_wh`, `power_w`, `last_meter_at`; frames and events as in [Event stream](#event-stream); faults list `remaining`, `faults`, each fault `id`, `scope`, `station`, `protocol`, `action`, `kind`, `remaining`; station rules list `station`, `rules` (each `action`, `protocol`, `payload`); one station rule `station`, `action`, `protocol`, `payload`.

## Start

```text
kirby-csms --listen 127.0.0.1:9000 --api-listen 127.0.0.1:9100
api listening on http://127.0.0.1:9100
```

The `api listening` line comes right after the `listening` line (with `--json`: `{"type":"api_listening","addr":"127.0.0.1:9100","url":"http://127.0.0.1:9100"}`). Port `0` picks a free port. The API works in the TUI, headless and `--script` modes and stops (gracefully, waiting at most 3 s for open requests) when the server stops. Flags: [configuration.md](configuration.md#listener-and-security). The API uses only the same engine handle as the stdin commands; it adds no new capabilities to the engine.

## Auth model

- Without a token the API accepts only loopback addresses (`--api-listen 0.0.0.0:...` without a token exits with code 2). Any local process of any user can use it.
  - Browsers are kept out by three checks, each answered with `403`: a request with an `Origin` header; a request with `Sec-Fetch-Site: cross-site`; and a `Host` header that is not a loopback IP literal (`127.0.0.1`, `[::1]`, `[0:0:0:0:0:0:0:1]`, any `127.x.y.z`), the address the API is bound to (when it is a specific address), or `localhost` / `localhost.` (case-insensitive), each with an optional port.
  - The `403` text lists the accepted values. The `Host` rule is what stops DNS rebinding: a page on `evil.example` rebound to 127.0.0.1 sends no `Origin` on a same-origin GET, but its `Host` is still `evil.example`. A request without any `Host` header is let through (no browser sends one).
  - Reaching the API by another name (an SSH tunnel to a host name, a reverse proxy) needs a token.
- With `--api-token TOKEN` / `--api-token-file FILE` (env `KIRBY_CSMS_API_TOKEN`, `KIRBY_CSMS_API_TOKEN_FILE`) every request, whatever the path, needs exactly one `Authorization: Bearer TOKEN` header (scheme case-insensitive, exactly one space, compared in constant time; two `Authorization` headers are refused), otherwise `401` with `WWW-Authenticate: Bearer`.
  - With a token the `Host` and `Origin` rules do not apply: the bearer proves intent. A token must be visible ASCII without spaces (startup exit code 2 otherwise).
  - `--api-token` is visible in `ps`; prefer the environment variable or the file (read once, trailing newline dropped, must not be empty; on unix a warning goes to stderr when the file is readable by group or others). `KIRBY_CSMS_API_TOKEN` and `--api-token-file` together are a usage error (exit 2), like `--api-token` with it.
- There is no per-user permission. Over plain HTTP the token is sent in clear text, so a plain listener belongs on loopback; see [TLS](#tls). Everything the API can do (sending any CALL to any station, forgetting stations) is available to every holder of the token.

## TLS

`--api-tls-cert FILE` and `--api-tls-key FILE` (env `KIRBY_CSMS_API_TLS_CERT`, `KIRBY_CSMS_API_TLS_KEY`; config keys `api_tls_cert`, `api_tls_key` in `[api]`) make the API listener serve HTTPS: a PEM certificate chain (leaf first) and one private key (PKCS#8, PKCS#1 or SEC1), the same formats as `--tls-cert` / `--tls-key` of the station listener, which the API does not share. The two flags come as a pair and need `--api-listen` (usage error, exit 2, otherwise). TLS 1.2 and 1.3, HTTP/1.1 only (ALPN `http/1.1`). The certificate and key are read at startup; `--check-config` loads them and prints nothing from them.

```text
kirby-csms --api-listen 127.0.0.1:9100 --api-tls-cert server.pem --api-tls-key server.key --api-token-file api.token
api listening on https://127.0.0.1:9100
```

```sh
curl --cacert ca.pem -H "Authorization: Bearer $(cat api.token)" https://localhost:9100/v1/stations
```

Which combinations start:

| `--api-listen` address | Token | TLS | Result |
| --- | --- | --- | --- |
| loopback | none | none | starts; the `Origin`, `Sec-Fetch-Site` and `Host` checks of the [auth model](#auth-model) apply |
| loopback | set | none | starts |
| loopback | none or set | set | starts; with TLS and no token the same browser checks apply |
| other (including `0.0.0.0`, `[::]`) | none | any | exit 2: a token is required |
| other | set | none | exit 2: the token would travel in clear text; set `--api-tls-cert` and `--api-tls-key` or listen on loopback |
| other | set | set | starts |

`--api-tls-client-ca FILE` (env `KIRBY_CSMS_API_TLS_CLIENT_CA`, key `api_tls_client_ca`) turns on mutual TLS: every client must present a certificate that chains to a CA in the file, otherwise the handshake fails and no request is read. It needs `--api-tls-cert`, and it adds to the token rather than replacing it. Client certificates are not mapped to identities.

```sh
curl --cacert ca.pem --cert client.pem --key client.key https://localhost:9100/v1/health
```

Handshakes: a client has 5 s to finish the TLS handshake, and at most 64 handshakes run at the same time; a connection that arrives over that number is closed at once. A failed, slow or silent handshake closes the connection without a log line. Handshakes in progress count against the 512-connection limit, not against the 256 command slots. A plain HTTP request to the TLS port gets no HTTP answer.

## Endpoints

JSON in and out, `Content-Type: application/json`. Errors have the body `{"error":{"code":"...","message":"..."}}`, see [Errors and limits](#errors-and-limits). Station ids in the path are percent-decoded; an id longer than 128 bytes or with control or bidi characters is `400`. Text from stations is escaped like the `--json` lines.

| Method and path | Meaning |
| --- | --- |
| `GET /v1` | Index: `{"api_version":1,"version":"...","resources":[{"path":"/v1/health","methods":["GET"]},...]}`, every route with its methods (`{id}` and `{action}` are path parameters) |
| `GET /v1/health` | `{"ok":true,"api_version":1,"version":"...","stations":N}` |
| `GET /v1/stations` | `{"stations":[...]}`, the station objects (below) |
| `GET /v1/stations/{id}` | The station object plus `state` (boot info, connectors, transactions, ...); `404` `unknown_station` if never seen |
| `GET /v1/stations/{id}/history` | `{"station":..,"frames":[...],"next_cursor":..,"gap":..}`, oldest first; query parameters `limit`, `since`, `action`, `kind`, `direction`, `format` (`json`, `ndjson`, `csv`); frames are shaped like the `--json` frame lines plus a `cursor`; `AuthorizationKey` values are redacted unless `--show-secrets`; `404` `unknown_station` if never seen. See [History, export and metrics](#history-export-and-metrics) |
| `GET /v1/transactions` | `{"transactions":[...],"truncated":..}`: the transactions of every station, with the filters `station`, `state`, `since`, `limit` and `format` (`json`, `csv`) |
| `GET /v1/metrics` | Counters and gauges in the Prometheus text format |
| `POST /v1/stations/{id}/calls` | Sends a CALL; body `{"action":"Reset","payload":{...},"timeout_ms":5000}` (`payload` default `{}`, `timeout_ms` 1..=86400000, default `--call-timeout`). Answers with `outcome` (`"result"`, `"call_error"` or `"timeout"`) and `ok` always, plus `result` for `result`, `error`, `code` and `details` for `call_error`, and `error` for `timeout` (the fields of the other outcomes are absent); `result` and `details` have `AuthorizationKey` values redacted unless `--show-secrets`; no `type`, `station` or `action` (the request names them), see below |
| `POST /v1/calls` | Sends one action to at most 64 stations selected by a glob and optional protocol; returns bounded per-station outcomes, sorted by station id. See [Bulk calls](#bulk-calls) |
| `DELETE /v1/stations/{id}` | Forgets a disconnected station (`{"station":..,"forgotten":true}`); `409` `conflict` while it is connected (decided atomically with the removal), `404` `unknown_station` if never seen |
| `GET /v1/events?station=ID` | Server-Sent Events, see [Event stream](#event-stream); resumes after the `Last-Event-ID` header or `last_event_id=ID` |
| `GET /v1/rules` | `{"rules":["Heartbeat",...],"protocol_rules":[{"action","protocol"},...]}`: `rules` lists the actions that have a fixed reply for every station, sorted; `protocol_rules` lists the global rules limited to one protocol (set by stdin, scripts, `[[rule]]` blocks or `PUT ?protocol=`), sorted by action, `[]` when none |
| `PUT /v1/rules/{action}` | Sets the fixed CALLRESULT payload (body: a JSON object) for a charge-point action. `?protocol=` limits the rule to stations that negotiated that protocol (`protocol_rules`) |
| `DELETE /v1/rules/{action}` | Removes the rule (`{"action":..,"cleared":true\|false}`); restores the default answer. Without `?protocol=` it removes the rule that applies to every station; with `?protocol=` it removes the global rule limited to that protocol, and only when its protocol matches |
| `GET /v1/stations/{id}/rules` | `{"station":..,"rules":[{"action","protocol","payload"}]}`: the rules of one station, sorted by action |
| `GET /v1/stations/{id}/rules/{action}` | `{"station","action","protocol","payload"}`; `404` `unknown_rule` when the station has no rule for the action |
| `PUT /v1/stations/{id}/rules/{action}` | Sets the fixed CALLRESULT payload (body: a JSON object) for one station; optional `?protocol=ocpp1.6` |
| `DELETE /v1/stations/{id}/rules/{action}` | Removes the station's rule (`{"station","action","cleared":true\|false}`); the global rule or the default answer applies again |
| `POST /v1/faults` | Injects a fault, see [Rules and faults](#rules-and-faults) |
| `GET /v1/faults` | `{"remaining":N,"faults":[...]}`: fault occurrences not triggered yet, and the stored faults with their ids |
| `DELETE /v1/faults` | Clears all faults of every scope (`{"remaining":0}`) |
| `DELETE /v1/faults/{id}` | Removes one fault (`{"id":..,"removed":true\|false}`) |

Status of a call: `200` also when the station answered with a CALLERROR or the call timed out (`"ok":false` and `outcome` `call_error` or `timeout` in the body); `404` `unknown_station` when the id was never seen; `409` `station_not_connected` when the station is known but has no open connection (or the connection closed before the answer); `400` for a bad body; `413` above 1 MiB; `429` `too_many_requests` when 256 commands (stdin and API together) are already in flight, or when the request bodies of the calls in flight already hold 32 MiB (a body counts while its call runs; only the payload is kept in memory, the rest of the body is dropped before the call starts).
- The 256 slots are shared with the stdin handler: when they are full the API answers `429` at once, while a stdin line waits for a free slot.

A station object always has these fields:

| Field | Value |
| --- | --- |
| `id` | Station id |
| `connected` | `true` while a connection is open |
| `protocol` | Negotiated subprotocol (string) or `null` |
| `received_calls` | Count of CALLs received from the station |
| `remote_addr` | Peer address (string) or `null` |
| `vendor`, `model` | Strings, `null` until the station sent a `BootNotification` |
| `connectors` | Connector key to status, `{}` until known. Keys are opaque strings: clients must not parse them or assume they are unique numbers. They are the decimal connector id in OCPP 1.6; a protocol where connector ids repeat per EVSE uses a composite key |
| `active_transactions` | List of `{"id","connector","evse"}`, `[]` until known. `connector` is the connector id of the protocol, `evse` the EVSE id or `null` (always `null` in OCPP 1.6, which has no EVSEs) |
| `security_events` | Count of security events, `0` until known |

`GET /v1/stations/{id}` adds `state` (`null` when the engine holds no state for the station).

The content of `state` (boot info, connectors, transactions) depends on `protocol`: the keys documented here and in `kirby-ocpp state show --json` are those of OCPP 1.6. Interpret `state` by the `protocol` of the station and do not rely on its keys across protocols; the other fields of the station object are protocol-neutral.

### Ids

Protocol ids are strings in the API, so that OCPP 2.x transaction ids (strings) fit the same shape as OCPP 1.6 ones (integers). `id` of `active_transactions[]` and of `state.transactions[]` is a string such as `"123"`, in station objects and in `state` events. Frames, call payloads and results are passed through as the protocol defines them (a 1.6 `transactionId` stays a number there). The stdin answers and the `--json` lines of `kirby-csms` keep the numbers of 0.4: only the API converts.

```text
$ curl -s http://127.0.0.1:9100/v1/stations
{"stations":[{"active_transactions":[],"connected":true,"connectors":{"1":"Available"},"id":"CP-1","model":"Emu","protocol":"ocpp1.6","received_calls":3,"remote_addr":"127.0.0.1:4242","security_events":0,"vendor":"kirby-ocpp"}]}

$ curl -s -X POST http://127.0.0.1:9100/v1/stations/CP-1/calls \
    -H 'Authorization: Bearer '"$TOKEN" \
    -d '{"action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"}}'
{"outcome":"result","ok":true,"result":{"status":"Accepted"}}

$ curl -s 'http://127.0.0.1:9100/v1/stations/CP-1/history?limit=5'
$ curl -s -X DELETE http://127.0.0.1:9100/v1/stations/CP-1
```

## Bulk calls

`POST /v1/calls` sends one action to a snapshot of matching known stations. The body has
`station` (required glob), `action` (required), `payload` (default `{}`), `protocol`
(optional negotiated-subprotocol filter) and `timeout_ms` (the single-call timeout range).
Unknown fields are refused. Station patterns match the whole id, case-sensitively:
`*` matches any sequence of characters and `?` matches one character. There are no escapes
or character classes. Patterns and action names are 1 to 128 UTF-8 bytes without control
or bidirectional characters. A protocol must be supported by the action registry.

The snapshot includes disconnected stations and is sorted by id. At most 64 stations may
match; a larger selection is `400 bad_request` before any CALL is sent. No matches returns
`200` with an empty `results` array. Slots for every selected station and the request size
multiplied by the selection size are reserved before dispatch, against the shared
256-command and 32 MiB budgets. Insufficient capacity returns `429 too_many_requests`
without sending a CALL. There are no automatic retries.

```json
{"station":"CP-*","protocol":"ocpp1.6","action":"Reset","payload":{"type":"Soft"},"timeout_ms":5000}
```

The response is `200` with `action` and `results`. Every result has `station`, `protocol`,
`outcome`, `ok`, `result`, `error`, `code` and `details`, with `null` for unused values.
`outcome` is `result`, `call_error`, `timeout` or `error`. Transport and admission failures
use the existing API error codes in `error: {"code","message"}`; CALLERROR diagnostics use
the same object, with the protocol error code in `code` and its details in `details`.
One station's failure does not discard other results. Result payloads and error details
larger than 64 KiB become `{"truncated":true,"originalBytes":N}`. Diagnostic messages are
limited to 1024 characters; protocol error codes are limited to 128 characters. Dropping a
request abandons its unfinished calls and releases its budget.

## Event stream

`GET /v1/events` is a Server-Sent Events stream of everything the server sees, from the moment of the request, or from the position a reconnecting client names (see [Resume](#resume)).
- Each SSE message has an `id:` line, an `event: <type>` line and one `data:` line. The `id:` is the number of the event in one log that every stream reads from: an event has the same `id:` on every stream, ids grow by one per event, and they start from the start time of the server in microseconds, so an id from an earlier run of the server is smaller than every id of the current one. Treat ids as opaque numbers that only compare equal or ascending. A stream with `station=` skips the ids of other stations' events.
- The `data:` line holds the same JSON object that `--json` prints for that event. Event types are `connected`, `disconnected`, `frame`, `state`, `malformed`, `forgotten` and `auth_failed`.
- Text is escaped the same way as in `--json`, and `AuthorizationKey` values are redacted unless `--show-secrets` is set (a `malformed` text that mentions `AuthorizationKey` is replaced by `<redacted: mentions AuthorizationKey>`).
- A `frame` event whose frame is larger than 64 KiB as JSON carries the payload marker `{"truncated":true,"originalBytes":N}` instead of the payload, like the frame history does above its own 8 MiB cap; `GET .../history` still returns the frame in full while it fits that cap.
- The optional `station=ID` (percent-encoded, at most once) shows only that station's events (and the notes below); an id that does not exist is fine and matches nothing. The only other parameter is `last_event_id`; a repeated or any other parameter is `400`. A new stream without a resume point replays nothing: use `/history` for the past.

```text
$ curl -N http://127.0.0.1:9100/v1/events?station=CP-1
id: 1
event: connected
data: {"protocol":"ocpp1.6","remote_addr":"127.0.0.1:4242","station":"CP-1","ts":"2030-01-01T10:00:00.000Z","type":"connected"}

id: 2
event: frame
data: {"action":"Heartbeat","direction":"in","id":"...","kind":"call","payload":{},"station":"CP-1","ts":"...","type":"frame"}

: keep-alive
```

- A `: keep-alive` comment is sent every 15 s.
- A client that reads too slowly loses events (it fell behind the log, see [Resume](#resume)) and gets `event: note` with `{"type":"note","text":"N events were dropped"}`; the stream continues with the oldest event that is still held. A note is also sent to every stream when the server itself lost events it could not read in time, whatever the `station=` filter.
- At most 16 streams at a time; the 17th gets `503` with a JSON error. The slot is freed when the client disconnects, when its socket stops accepting writes for 30 s or fails (TCP keepalive: first probe after 30 s idle, then every 10 s, 3 probes, where the platform allows it), or when the stream reaches its maximum age of 1 hour: the last event is then `event: note` with `{"type":"note","text":"the stream reached its maximum age; reconnect to continue"}` and the stream ends. A client that is connected but never reads is only cut by that age limit, since its kernel keeps acknowledging.
- After a lag note or a `resync`, read the state again (`GET /v1/stations`): a `state` event is sent only when the view of a station changed, so events the client missed are not repeated.
- Streams end when the server stops, after the closing events of the shutdown (`disconnected` with reason `shutdown`) were sent; the API stops after the CSMS. The token and `Origin` rules apply as for every route (`curl -N -H "Authorization: Bearer $TOKEN" ...`).

### Resume

A client that lost its connection asks for the events it missed with the `Last-Event-ID` header (what `EventSource` sends by itself) or, for a client that cannot set headers, the query parameter `last_event_id=ID`. The header wins when both are sent.
- The server keeps the newest events in a ring: at most 1024 events and 8 MiB of event text. A stream resumes with exactly the events after `ID` that are still held, in order, once each, then continues with live events. Nothing is missed or repeated in between. With `station=`, only that station's events are replayed.
- The event after `ID` must still be held. When `ID` is older than that, newer than the newest event (an id of an earlier run counts as older), or a header value that is not a decimal number, the stream starts with `event: resync` (`{"type":"resync","text":"..."}`) and then continues live without a replay. The `id:` of a `resync` is the id of the newest event at that moment, so the id a client keeps stays valid. On `resync` read the state again (`GET /v1/stations`, `GET /v1/stations/{id}/history`). A malformed `last_event_id` query value is `400`; a malformed header value is not (a client that reconnects by itself could not fix it).
- A `note` (slow reader, maximum age) carries the id of the last event the stream has accounted for, not a new one, so reconnecting with the id of a note resumes right after that point.
- The ring is memory only: a restarted server holds no events, and every older id gets a `resync`.

```text
$ curl -N -H 'Last-Event-ID: 1794161964006230' http://127.0.0.1:9100/v1/events
id: 1794161964006231
event: frame
data: {"direction":"in","id":"csms-2","kind":"result","payload":{...},"station":"CP-1","ts":"...","type":"frame"}

$ curl -N -H 'Last-Event-ID: 1' http://127.0.0.1:9100/v1/events
id: 1794161964006229
event: resync
data: {"text":"the requested event id is no longer held; read the current state and continue from here","type":"resync"}
```

## History, export and metrics

All of these are read-only and need the same token as every other route.

### History

`GET /v1/stations/{id}/history` returns the frames the server holds for a station (the newest `--history-cap` frames, 1000 by default, within 8 MiB), oldest first.

| Parameter | Value |
| --- | --- |
| `limit` | Number of frames: ASCII decimal digits (`+5`, `-1` and an empty value are `400`). Default: every held frame. Without `since` the newest `limit` frames, with `since` the first `limit` frames after the cursor. `0` gives none |
| `since` | A `cursor` from an earlier answer (`c` and decimal digits): only frames recorded after that frame |
| `action` | Only frames of CALLs of this action (1 to 128 bytes without control characters) and the replies to them |
| `kind` | `call`, `result` or `error` |
| `direction` | `in` (from the station) or `out` (to the station) |
| `format` | `json` (default), `ndjson` or `csv` |

Every value is percent-decoded like `station=`. An unknown parameter, a repeated one, a value outside the table, a `since` that is no cursor or is newer than any frame of this run is `400` `bad_request`. Filters combine with AND.

```text
$ curl -s 'http://127.0.0.1:9100/v1/stations/CP-1/history?since=c0&limit=2&kind=call'
{"frames":[{"action":"BootNotification","cursor":"c1794161964005842","direction":"in","id":"4039ce3d-f072-421f-af29-e654b802a973","kind":"call","payload":{"chargePointModel":"kirby-ocpp","chargePointVendor":"archhaze24"},"station":"CP-1","ts":"2026-11-08T18:19:25.023Z","type":"frame"},{"action":"StatusNotification","cursor":"c1794161964005844",...}],"gap":false,"next_cursor":"c1794161964005844","station":"CP-1"}
```

- Each frame has a `cursor` besides the keys of an event frame. A cursor is an opaque string that stays valid while the server runs: it names a position, not a count, so it survives the eviction of older frames. Cursors increase with recording order across all stations, and a cursor from an earlier run of the server is simply older than every frame of this run. Do not parse it.
- `action` filters by the action of the CALL: a `result` or `error` frame has no action of its own, so it is matched with the CALL of the same message id that went the other way. A reply whose CALL was already evicted is not matched.
- `next_cursor` is where to continue: pass it as `since` to get the frames after this answer, never one twice. It is the cursor of the last frame returned when `limit` cut the answer short, otherwise the cursor of the newest frame held (every frame up to it was looked at, whether it passed the filters or not), otherwise the `since` of the request. It is `null` only when the station holds no frames and no `since` was sent. An empty answer keeps `since` as `next_cursor`, so a polling client always keeps a valid position.
- `gap` is `true` when `since` was sent and frames after it were already evicted from the station's history (or were too big to be kept): the answer then starts with the oldest frame still held. Without `since` it is `false`.
- Walk a long history with `since` and `limit`: start with `since=c0`, then repeat with `next_cursor` until `frames` is empty. Poll for new traffic the same way.
- `format=ndjson` is one frame object per line (the same objects, escaped like every JSON body, `Content-Type: application/x-ndjson`) and `format=csv` is a CSV document (see [CSV](#csv)) with the columns `cursor`, `ts`, `station`, `direction`, `kind`, `id`, `action`, `payload`, `code`, `description`, `details` (`payload` and `details` as compact JSON text; a column that does not apply to a frame is empty). Neither format has an envelope: to continue, use the `cursor` of the last row as `since`.
- The size of an answer is bounded by the history itself (at most `--history-cap` frames and 8 MiB of frames), so there is no separate cap.

```text
$ curl -si 'http://127.0.0.1:9100/v1/stations/CP-1/history?format=csv&limit=2'
HTTP/1.1 200 OK
content-type: text/csv; charset=utf-8
...

cursor,ts,station,direction,kind,id,action,payload,code,description,details
c1794161964005848,2026-11-08T18:19:25.039Z,CP-1,in,call,b69364b7-eba9-4e42-bd14-c2436c4b4787,SecurityEventNotification,"{""timestamp"":""2026-11-08T18:19:25.037695010Z"",""type"":""StartupOfTheDevice""}",,,
c1794161964005849,2026-11-08T18:19:25.039Z,CP-1,out,result,b69364b7-eba9-4e42-bd14-c2436c4b4787,,{},,,
```

### Transactions

`GET /v1/transactions` lists the transactions the server tracks (per station, the newest `--transaction-history-cap`, 100 by default), ordered by station id and then as the station holds them.

| Parameter | Value |
| --- | --- |
| `station` | Only this station (exact id, 1 to 128 bytes without control characters); an id that does not exist matches nothing |
| `state` | `active` or `stopped` (the values of the `status` field) |
| `since` | RFC 3339 timestamp: only transactions that started at or after it, by the station's clock. A `+` in an offset must be written `%2B`; `Z` is simplest |
| `limit` | At most this many transactions, 0 to 10000, default 1000 |
| `format` | `json` (default) or `csv` |

An answer is `{"transactions":[...],"truncated":false}`. Each transaction has `station`, `id` (a string), `connector`, `id_tag`, `id_tag_status`, `meter_start`, `started_at`, `status` (`active` or `stopped`), `meter_stop`, `stopped_at`, `stop_reason`, `energy_wh`, `power_w` and `last_meter_at`; a value that is not known yet is `null`. `truncated` is `true` when more transactions matched than `limit`: narrow the filters or ask for the stations one by one. With the default fixed transaction id policy several transactions can share an id; `station` plus `started_at` tell them apart. The `csv` format has the same fields as columns in this order: `station`, `id`, `connector`, `id_tag`, `id_tag_status`, `meter_start`, `started_at`, `status`, `meter_stop`, `stopped_at`, `stop_reason`, `energy_wh`, `power_w`, `last_meter_at`, and the response header `X-Truncated: true|false`. Bad values are `400` `bad_request` like for the history.

```text
$ curl -s http://127.0.0.1:9100/v1/transactions
{"transactions":[{"connector":1,"energy_wh":null,"id":"123","id_tag":"TAG","id_tag_status":"Accepted","last_meter_at":null,"meter_start":0,"meter_stop":null,"power_w":null,"started_at":"2026-11-08T18:19:42.397963001Z","station":"CP-1","status":"active","stop_reason":null,"stopped_at":null}],"truncated":false}
$ curl -s 'http://127.0.0.1:9100/v1/transactions?state=active&format=csv'
station,id,connector,id_tag,id_tag_status,meter_start,started_at,status,meter_stop,stopped_at,stop_reason,energy_wh,power_w,last_meter_at
CP-1,123,1,TAG,Accepted,0,2026-11-08T18:19:42.397963001Z,active,,,,,,
```

### CSV

CSV answers follow RFC 4180: a header row, CRLF line ends, a cell with a comma or a double quote in double quotes (a quote doubled), `Content-Type: text/csv; charset=utf-8`. Text in a cell comes from stations and is not trusted:
- Control and bidirectional characters (the ones `--json` escapes) are replaced by U+FFFD.
- A text cell that starts with `=`, `+`, `-` or `@` gets a leading `'`, so a spreadsheet does not read it as a formula (`=CP1` becomes `'=CP1`). Cells that hold a number (`connector`, `meter_start`, `meter_stop`, `energy_wh`, `power_w`) are written as the number and are never prefixed; a negative meter value stays `-5`.
- Secrets are redacted exactly as in the JSON history (`AuthorizationKey` values, unless `--show-secrets`).

### Metrics

`GET /v1/metrics` answers in the Prometheus text exposition format 0.0.4 (`Content-Type: text/plain; version=0.0.4; charset=utf-8`). The names and the labels below are part of the `/v1` contract; new metrics may be added.

| Metric | Type | Meaning |
| --- | --- | --- |
| `kirby_csms_uptime_seconds` | gauge | Seconds since the API started |
| `kirby_csms_stations_known` | gauge | Stations in the registry, connected or not |
| `kirby_csms_stations_connected` | gauge | Stations with an open connection |
| `kirby_csms_faults_active` | gauge | Stored faults not used up |
| `kirby_csms_fault_occurrences_remaining` | gauge | Fault occurrences not triggered yet |
| `kirby_csms_rules_global` | gauge | Global response rules |
| `kirby_csms_rules_station` | gauge | Per-station response rules |
| `kirby_csms_history_frames_dropped_total` | counter | Frames that left a station history by eviction or because they were too big |
| `kirby_csms_events_dropped_total` | counter | Engine events the event log missed because it fell behind |
| `kirby_csms_stream_events_dropped_total` | counter | Events that event streams skipped because they fell behind the log |
| `kirby_csms_event_streams` | gauge | Open event streams |
| `kirby_csms_calls_received_total{action}` | counter | CALLs received from stations |
| `kirby_csms_calls_sent_total{action}` | counter | CALLs sent to stations |
| `kirby_csms_api_requests_total{class}` | counter | API responses by status class (`1xx` to `5xx`) |
| `kirby_csms_build_info{version}` | gauge | Always `1`, the version is the label |

- The `action` label takes only action names from the server's protocol tables (the actions a station sends and the ones the server can send); every other name is counted under `other`, so a station cannot grow the number of series. The station id is never a label.
- Counters count from the start of the API and reset on restart. `calls_*` count the CALL frames that crossed the wire, `api_requests_total` the responses of the router (a request that hyper rejects before it reaches the router, such as a malformed head, is not counted).
- Faults, rules, stations and streams are read when the endpoint is scraped.

```text
$ curl -s http://127.0.0.1:9100/v1/metrics | grep -v '^#'
kirby_csms_uptime_seconds 23
kirby_csms_stations_known 1
kirby_csms_stations_connected 1
...
kirby_csms_calls_received_total{action="BootNotification"} 1
kirby_csms_calls_received_total{action="StatusNotification"} 4
kirby_csms_calls_sent_total{action="RemoteStartTransaction"} 1
kirby_csms_api_requests_total{class="2xx"} 12
kirby_csms_api_requests_total{class="4xx"} 1
kirby_csms_build_info{version="0.6.0-dev"} 1
```

With a token, give the scraper the bearer token (`authorization` in the Prometheus scrape config, see [cookbook.md](cookbook.md)).

## Rules and faults

Same engine features as the script steps `respond` and `inject_fault` ([csms.md](csms.md#response-rules-and-fault-injection)). The endpoints under `/v1/rules` and `POST /v1/faults` without `station` apply to every station; the per-station endpoints and the optional `station` and `protocol` fields scope a rule or fault to one station, one protocol or both.

**Resolution order** for one CALL from a station, first match wins:
1. a fault of that station (injection order), then a global fault (injection order); a fault with a `protocol` applies only to stations that negotiated it; one CALL uses up one occurrence of one fault;
2. schema validation, when enabled;
3. the rule of that station for the action (a `protocol` on the rule must match the station's protocol);
4. a global rule that names a protocol (set from a `[[rule]]` block of the configuration file), when the protocol matches;
5. the global rule of `/v1/rules` without a protocol (one per action; setting it again replaces it);
6. the default answer.

Faults beat rules at every scope. `times` counts per fault: two faults for the same action and scope are used one after the other. A station that negotiated no subprotocol (`"protocol":null`) matches only scopes without a `protocol`. Rules and faults of a station stay when the station is forgotten and apply when it connects again; the station id does not have to be known yet.

```text
$ curl -s -X PUT http://127.0.0.1:9100/v1/rules/Heartbeat -d '{"currentTime":"2030-01-01T00:00:00Z"}'
{"action":"Heartbeat","set":true}
$ curl -s http://127.0.0.1:9100/v1/rules
{"protocol_rules":[],"rules":["Heartbeat"]}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/rules/Heartbeat
{"action":"Heartbeat","cleared":true}

$ curl -s -X POST http://127.0.0.1:9100/v1/faults -d '{"action":"Heartbeat","kind":"call_error","times":2}'
{"id":"f1","injected":true,"remaining":2}
$ curl -s http://127.0.0.1:9100/v1/faults
{"faults":[{"action":"Heartbeat","id":"f1","kind":"call_error","protocol":null,"remaining":2,"scope":"global","station":null}],"remaining":2}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/faults
```

Per station:

```text
$ curl -s -X PUT http://127.0.0.1:9100/v1/stations/CP-1/rules/Authorize -d '{"idTagInfo":{"status":"Blocked"}}'
{"action":"Authorize","protocol":null,"set":true,"station":"CP-1"}
$ curl -s http://127.0.0.1:9100/v1/stations/CP-1/rules
{"rules":[{"action":"Authorize","payload":{"idTagInfo":{"status":"Blocked"}},"protocol":null}],"station":"CP-1"}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/stations/CP-1/rules/Authorize
{"action":"Authorize","cleared":true,"station":"CP-1"}

$ curl -s -X POST http://127.0.0.1:9100/v1/faults -d '{"action":"StartTransaction","kind":"silence","station":"CP-1","protocol":"ocpp1.6","times":3}'
{"id":"f2","injected":true,"remaining":3}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/faults/f2
{"id":"f2","removed":true}
```

- `PUT /v1/stations/{id}/rules/{action}`: the same checks as the global endpoint on `{action}` and the body. The optional query parameter `protocol` (at most once, no other parameter) limits the rule to stations that negotiated that subprotocol; it must be a protocol the server has an action table for (`ocpp1.6` today, `400` `bad_request` otherwise) and `{action}` must be an action a station of that protocol sends. Without it the rule applies whatever the protocol, and `{action}` must be an action of any known protocol. The answer repeats `station`, `action` and `protocol` (`null` when not given). Setting the rule again replaces it, with the new `protocol`. `{id}` is at most 128 bytes without control characters (`400` otherwise).
- `GET`, `DELETE` of one rule use the same `{id}` and `{action}` checks. A `GET` of a station or action without a rule is `404` `unknown_rule`; `DELETE` is idempotent (`cleared` tells whether a rule was removed) and takes no query parameter (`400`). `GET /v1/stations/{id}/rules` is `200` with `"rules":[]` for a station that has none or that was never seen.
- `GET /v1/faults`: `remaining` is the sum of the occurrences left in every scope (as before). `faults` lists the stored faults in injection order, each with `id`, `scope` (`"global"` or `"station"`), `station` (the id, `null` for a global fault), `protocol` (`null` when not limited), `action`, `kind` and `remaining` (occurrences left, 1 or more). A fault disappears from the list when its last occurrence is used.
- Fault ids are opaque strings (`f1`, `f2`, ...). An id stays valid until the fault is used up or removed, is never reused during one run and carries no meaning clients may rely on beyond equality. `DELETE /v1/faults/{id}` with an id that is not stored answers `200` with `"removed":false` (a fault can be used up between the list and the delete); it removes the fault with its remaining occurrences.
- `POST /v1/faults` accepts two more optional fields, `station` (exact station id, 1 to 128 bytes without control characters) and `protocol` (a protocol with an action table). The answer carries the new `id` besides `injected` and `remaining`.
- Caps (a request beyond them is `409` `conflict` with a JSON error and changes nothing): 1000 faults in all, 100 faults for one station, 8 MiB of action and payload text over all faults; 1000 scoped rules in all, 64 rules for one station, 8 MiB of payload text over all scoped rules. Scoped rules are the per-station rules and the global rules that carry a protocol, so the 1000 and the 8 MiB count both kinds; only per-station rules count toward the 64. A rule that replaces an existing one does not count as new. A global rule without a protocol (`/v1/rules/{action}`) is one per action and has no cap. A global rule with a protocol is also one per action: setting it with another protocol replaces it.

- `PUT /v1/rules/{action}`: `{action}` must be an action a charge point can send in the protocol of the engine's action table, today `ocpp1.6`, or in the `?protocol=` given (otherwise `400` `unsupported_action`; an unknown `protocol` is `400` `bad_request`; a request beyond the caps is `409`); the body must be a JSON object (the whole CALLRESULT payload, not checked against the schema).
- `POST /v1/faults` body: `{"action","kind","payload"?,"times"?}` with `kind` one of `call_error`, `silence`, `close`, `invalid_payload` (needs `payload`, sent as the CALLRESULT as given); `action` is 1 to 128 bytes without control characters (any other name is accepted, so faults can target actions the server does not know; `400` otherwise); `times` defaults to 1 and must be 1 to 1000000 (`400` otherwise); the caps listed above apply (`409` with a JSON error beyond that; `DELETE /v1/faults` frees them). Scripts apply the same checks and caps (an `inject_fault` step fails the script when the store is full); the terminal UI form is bounded by the same `times` limit. Faults are used in the order injected and take precedence over rules. Unknown fields are `400`.

## Command-line client

`kirby-csms ctl` calls the API from a shell script or a CI job. It sends the requests of the [OpenAPI document](kirby-csms-openapi.json), prints the JSON the API returns and reports the result in its exit code. It needs no server flags and starts no server. Every flag, environment variable and exit code: [configuration.md](configuration.md#kirby-csms-ctl).

```sh
export KIRBY_CSMS_API_URL=https://csms.example:9100   # default http://127.0.0.1:9100
export KIRBY_CSMS_API_TOKEN_FILE=api.token            # or KIRBY_CSMS_API_TOKEN; none for an open API
export KIRBY_CSMS_API_CA_FILE=ca.pem                  # when the API certificate has a private CA
```

The examples show the output as written to a pipe or a file; on a terminal the JSON is indented.

```text
$ kirby-csms ctl stations
{"stations":[{"active_transactions":[],"connected":true,"connectors":{"0":"Available","1":"Available"},"id":"CP-1","model":"kirby-ocpp","protocol":"ocpp1.6","received_calls":4,"remote_addr":"127.0.0.1:36514","security_events":1,"vendor":"archhaze24"}]}

$ kirby-csms ctl call CP-1 ChangeConfiguration --payload '{"key":"HeartbeatInterval","value":"60"}'
{"ok":true,"outcome":"result","result":{"status":"Accepted"}}

$ kirby-csms ctl call CP-1 NoSuchAction; echo "exit $?"
{"code":"NotImplemented","details":{},"error":"CALLERROR NotImplemented: NoSuchAction is not an OCPP 1.6J action","ok":false,"outcome":"call_error"}
exit 4

$ kirby-csms ctl bulk 'CP-*' Reset --payload '{"type":"Soft"}'
{"action":"Reset","results":[{"code":null,"details":null,"error":null,"ok":true,"outcome":"result","protocol":"ocpp1.6","result":{"status":"Accepted"},"station":"CP-1"}]}

$ kirby-csms ctl station nobody; echo "exit $?"
{"error":{"code":"unknown_station","message":"unknown station"}}
exit 1

$ kirby-csms ctl export history CP-1 --as csv --kind call --output history.csv
$ kirby-csms ctl faults add Heartbeat --kind call_error --times 2
{"id":"f1","injected":true,"remaining":2}
$ kirby-csms ctl events --station CP-1 --with-id --count 2
{"id":"1794579973220481","event":"frame","data":{"action":"Heartbeat","direction":"in",...}}
```

Commands: `health`, `index`, `stations`, `station`, `forget`, `call`, `bulk`, `history`, `transactions`, `rules list|show|set|clear`, `faults list|add|remove|clear`, `metrics`, `events` and `export history|transactions`; each is one request of the [endpoint table](#endpoints) (the route of every command is in the [command table](configuration.md#ctl-commands)).

- **Exit codes.** `0` success, `1` the API answered with an error status (the error body is on stderr), `2` usage error, `3` the API could not be reached or its answer is unusable, `4` a `call` or `bulk` reached the API but a station answered with a CALLERROR or not at all (`"ok":false`), or a `bulk` pattern matched no station (`no station matched the pattern` on stderr, the empty answer on stdout). A script can tell a refused request from a failed call from a missing server.
- **Token.** It comes from `KIRBY_CSMS_API_TOKEN` or a token file; there is no token flag, since flag values are visible in `ps`. It is never printed, not even in error messages. `--payload` is visible in `ps` as well: use `--payload-file` (`-` reads stdin) for a payload that holds a secret.
- **Output.** JSON is printed on one line as the API sent it (indented with `--pretty` and on a terminal). Text that came from stations is untrusted: characters that could drive a terminal are written as escapes, in JSON as `\uXXXX`, so the output stays valid JSON. Station ids and filter values are percent-encoded in the request.
- **Following events.** `events` prints one JSON line per event and reconnects with `Last-Event-ID` when the stream ends. Once an event with an id has been printed (or `--last-event-id` was given), the reconnect resumes after it, so the tail has no gaps and no repeats within what the server holds (see [Resume](#resume)). Before that, the server has sent no id yet: a reconnect starts from the live stream, events sent while the connection was down are lost, and `events` prints a note on stderr. A `200` answer that is not an event stream (`Content-Type: text/event-stream`) is a transport error. To continue in a later run, print the ids with `--with-id` and pass the last one as `--last-event-id`. `--count N` ends the run after N events, `--once` disables reconnecting.
- **Limits.** One request per connection, like the server's own limit. An answer larger than 64 MiB is refused. Request bodies follow the limits of the [call endpoints](#endpoints).

## Errors and limits

Every error the API produces has the body `{"error":{"code":"<code>","message":"<text>"}}` and `Content-Type: application/json`. The `code` is stable (snake_case); the `message` is for people and may change. One exception: what hyper answers before the request reaches the router has an empty body and no `Content-Type`: `400` for a malformed request line or head (and for an invalid `Host`/header syntax hyper rejects), `431` for a head that is too large, and a connection closed without any answer when the head is not complete within the head timeout. Clients must not parse the body of those answers.

| Status | `code` | When |
| --- | --- | --- |
| 400 | `bad_request` | Invalid JSON or fields, bad `timeout_ms`, bad `limit`, `since`, `action`, `kind`, `direction`, `state` or `format`, a malformed `last_event_id`, unknown or repeated query parameter, unsafe or over-long station id, more than one `Host` header, bad fault, an unknown `protocol`, a rule body that is not an object |
| 400 | `unsupported_action` | `PUT`/`DELETE` of `/v1/rules/{action}` or `/v1/stations/{id}/rules/{action}`, and `GET` of the latter, with an action that is not a known charge-point action for the protocol (`ocpp1.6` today) |
| 401 | `unauthorized` | Token configured and missing or wrong |
| 403 | `forbidden` | No token configured and the request has an `Origin` header, `Sec-Fetch-Site: cross-site` or a non-loopback `Host` |
| 404 | `not_found` | Unknown route |
| 404 | `unknown_rule` | `GET /v1/stations/{id}/rules/{action}` for a station that has no rule for the action |
| 404 | `unknown_station` | The station id was never seen (or was forgotten) |
| 405 | `method_not_allowed` | Wrong method for the path |
| 408 | `request_timeout` | Request body not complete 10 s after the head |
| 409 | `station_not_connected` | A call to a station that is known but not connected |
| 409 | `conflict` | Forgetting a connected station; fault store or per-station rule store full |
| 413 | `payload_too_large` | Request body over 1 MiB (also when chunked) |
| 429 | `too_many_requests` | Too many commands in flight, too much request data held by calls in flight, or 1024 calls already waiting for the station |
| 500 | `internal` | A call failed in a way that is neither a protocol outcome nor a request problem (for example an unexpected frame as the answer) |
| 503 | `too_many_streams` | Already 16 event streams open |

New codes may be added within `/v1`; clients must treat an unknown `code` by its status.

Connection limits: at most 512 connections at once (extra ones are closed immediately; more than the 256 command slots plus 16 streams, so `429` stays reachable); a client has 10 s to send the complete request head (hyper's `header_read_timeout`: it starts at connect, blank lines before the request line do not stop it, any line ending is accepted; the connection is cut otherwise) and 10 s to send the body (`408`).
- Every response produced by a route carries `Connection: close`, so a connection serves one request (hyper's own `400`/`431` answers and cuts by the head timeout do not carry the header).
- A call waits as long as its `timeout_ms`, which also counts the time it spends queued behind other calls of the same station; once the answer is ready its writing has a total limit of 60 s from the first byte (a client draining a byte at a time is cut), and a write that makes no progress for 30 s ends the connection.
- An event stream is exempt from the 60 s limit (see above). After a response the server shuts down its side and reads, for up to 100 ms and 64 KiB, what the client is still sending. This lets an early `401`, `403` or `413` reach a client that is mid-upload.
- A request body is buffered in memory before the request is routed, up to 1 MiB per connection; with 512 connections open at once the API can hold up to 512 MiB of request bodies. The 32 MiB budget above applies only to the payloads of calls in flight.
- More than one `Host` header is `400`. On shutdown the listener closes at once, open connections get 3 s, then every one is dropped (closed) and awaited: none outlives the API.

## Related pages

[CSMS](csms.md) · [Configuration](configuration.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
