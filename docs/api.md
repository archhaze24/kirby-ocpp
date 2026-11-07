# HTTP control API

Drive a running `kirby-csms` from any language or CI without stdin plumbing. Local, JSON, plain HTTP, off by default.

Contents: [Contract](#contract) · [Start](#start) · [Auth model](#auth-model) · [Endpoints](#endpoints) · [Event stream](#event-stream) · [Rules and faults](#rules-and-faults) · [Errors and limits](#errors-and-limits) · [Related pages](#related-pages)

## Contract

The shapes in this page are a public contract, versioned by the path prefix and by `api_version` (`GET /v1` and `GET /v1/health` return it; now `1`).
- Stable within `/v1`: the paths and methods in the table below, the field names and types of every JSON body, the error `code` values, the SSE event types and their `id:` lines. Additive changes (new fields, new event types, new endpoints, new error codes) keep `/v1`: clients must ignore fields and event types they do not know. A change that removes or renames a field, changes a type, or changes the meaning of a status or code is breaking and ships as `/v2` (with `/v1` kept for a release where feasible).
- Not part of the contract: the text of `message` fields and of `error` strings in call answers, the order of object keys, the order of stations in a list, and the `ts` precision.
- The API is protocol-neutral. A station object carries `protocol`, the WebSocket subprotocol negotiated for its connection (`"ocpp1.6"` today, `null` when the subprotocol policy selected none), so that stations of other OCPP versions can be served on the same listener without a new API version. Protocol ids are strings in the API (see [Ids](#ids)).
- Protocol-level outcomes of a call (the station answered with a CALLERROR, or did not answer in time) are HTTP `200` with `"ok":false`; HTTP errors mean the request was not delivered or was not valid, see [Errors and limits](#errors-and-limits).
- Browsers are refused by design: there are no CORS headers, and a request with an `Origin` header is refused without a token. `OPTIONS` answers `204` with an `Allow` header for every known path (behind the same token and `Host` checks) and is meant for tools, not for browser preflights.
- `Allow` is derived from the route table that `GET /v1` lists and appears only on a `405` and on the `204` answer to `OPTIONS`, in one format: the route's methods plus `HEAD` (for a `GET` route) and `OPTIONS`, sorted, joined by a comma and a space (`GET, HEAD, OPTIONS`). No other status (`200`, `401`, `403`, `404`, ...) carries it.
- Absent and `null`: a field that exists in a shape is always present. A value that is not known yet is an explicit `null` (a station that never booted has `"vendor":null`), a collection that is empty is `[]` or `{}`, a counter is `0`. A key that is missing means the field does not exist in this version of the API: clients must tolerate keys they do not know and must not rely on a key being absent. Optional extension fields that are not defined yet are simply not sent.
- Golden shapes (top-level keys, checked by the test suite): health `ok`, `api_version`, `version`, `stations`; index `api_version`, `version`, `resources` (each `path`, `methods`); station object (in the list and in `GET /v1/stations/{id}`) `id`, `connected`, `protocol`, `received_calls`, `remote_addr`, `vendor`, `model`, `connectors`, `active_transactions`, `security_events`, and `state` in `GET /v1/stations/{id}` only; history `station`, `frames`; frames and events as in [Event stream](#event-stream).

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
- There is no TLS and no per-user permission: the token is sent in clear text. Keep the API on loopback, or put it behind a reverse proxy or SSH tunnel that adds TLS. Everything the API can do (sending any CALL to any station, forgetting stations) is available to every holder of the token.

## Endpoints

JSON in and out, `Content-Type: application/json`. Errors have the body `{"error":{"code":"...","message":"..."}}`, see [Errors and limits](#errors-and-limits). Station ids in the path are percent-decoded; an id with control or bidi characters is `400`. Text from stations is escaped like the `--json` lines.

| Method and path | Meaning |
| --- | --- |
| `GET /v1` | Index: `{"api_version":1,"version":"...","resources":[{"path":"/v1/health","methods":["GET"]},...]}`, every route with its methods (`{id}` and `{action}` are path parameters) |
| `GET /v1/health` | `{"ok":true,"api_version":1,"version":"...","stations":N}` |
| `GET /v1/stations` | `{"stations":[...]}`, the station objects (below) |
| `GET /v1/stations/{id}` | The station object plus `state` (boot info, connectors, transactions, ...); `404` `unknown_station` if never seen |
| `GET /v1/stations/{id}/history?limit=N` | `{"station":..,"frames":[...]}`, oldest first, only the newest `N` with `limit` (`0` gives none; ASCII decimal digits only (`+5`, `-1` and an empty value are `400`), `%XX` escapes are decoded like `station=`; other or repeated parameters are `400`); frames are shaped like the `--json` frame lines; `AuthorizationKey` values are redacted unless `--show-secrets`; `404` `unknown_station` if never seen |
| `POST /v1/stations/{id}/calls` | Sends a CALL; body `{"action":"Reset","payload":{...},"timeout_ms":5000}` (`payload` default `{}`, `timeout_ms` 1..=86400000, default `--call-timeout`). Answers with `outcome` (`"result"`, `"call_error"` or `"timeout"`), `ok` and `result` or `error` (plus `code`, `details` for a CALLERROR); no `type`, `station` or `action` (the request names them), see below |
| `DELETE /v1/stations/{id}` | Forgets a disconnected station (`{"station":..,"forgotten":true}`); `409` `conflict` while it is connected (decided atomically with the removal), `404` `unknown_station` if never seen |
| `GET /v1/events?station=ID` | Server-Sent Events, see [Event stream](#event-stream) |
| `GET /v1/rules` | `{"rules":["Heartbeat",...]}`: actions that have a fixed reply, sorted |
| `PUT /v1/rules/{action}` | Sets the fixed CALLRESULT payload (body: a JSON object) for a charge-point action |
| `DELETE /v1/rules/{action}` | Removes the rule (`{"action":..,"cleared":true\|false}`); restores the default answer |
| `POST /v1/faults` | Injects a fault, see [Rules and faults](#rules-and-faults) |
| `GET /v1/faults` | `{"remaining":N}`: fault occurrences not triggered yet |
| `DELETE /v1/faults` | Clears all faults (`{"remaining":0}`) |

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
| `connectors` | Connector id to status, `{}` until known |
| `active_transactions` | List of `{"id","connector"}`, `[]` until known |
| `security_events` | Count of security events, `0` until known |

`GET /v1/stations/{id}` adds `state` (`null` when the engine holds no state for the station).

### Ids

Protocol ids are strings in the API, so that OCPP 2.x transaction ids (strings) fit the same shape as OCPP 1.6 ones (integers). `id` of `active_transactions[]` and of `state.transactions[]` is a string such as `"123"`, in station objects and in `state` events. Frames, call payloads and results are passed through as the protocol defines them (a 1.6 `transactionId` stays a number there). The stdin answers and the `--json` lines of `kirby-csms` keep the numbers of 0.4: only the API converts.

```text
$ curl -s http://127.0.0.1:9100/v1/stations
{"stations":[{"active_transactions":[],"connected":true,"connectors":{"1":"Available"},"id":"CP-1","model":"Emu","protocol":"ocpp1.6","received_calls":3,"remote_addr":"127.0.0.1:4242","security_events":0,"vendor":"Kirby"}]}

$ curl -s -X POST http://127.0.0.1:9100/v1/stations/CP-1/calls \
    -H 'Authorization: Bearer '"$TOKEN" \
    -d '{"action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"}}'
{"outcome":"result","ok":true,"result":{"status":"Accepted"}}

$ curl -s 'http://127.0.0.1:9100/v1/stations/CP-1/history?limit=5'
$ curl -s -X DELETE http://127.0.0.1:9100/v1/stations/CP-1
```

## Event stream

`GET /v1/events` is a Server-Sent Events stream of everything the server sees, from the moment of the request.
- Each SSE message has an `id:` line, an `event: <type>` line and one `data:` line. The `id:` is a counter that increases for every message the server sends on any stream, starting at 1.
- The `data:` line holds the same JSON object that `--json` prints for that event. Event types are `connected`, `disconnected`, `frame`, `state`, `malformed`, `forgotten` and `auth_failed`.
- Text is escaped the same way as in `--json`, and `AuthorizationKey` values are redacted unless `--show-secrets` is set (a `malformed` text that mentions `AuthorizationKey` is replaced by `<redacted: mentions AuthorizationKey>`).
- A `frame` event whose frame is larger than 64 KiB as JSON carries the payload marker `{"truncated":true,"originalBytes":N}` instead of the payload, like the frame history does above its own 8 MiB cap; `GET .../history` still returns the frame in full while it fits that cap.
- `Last-Event-ID` is ignored: a reconnecting client gets no replay and the counter does not restart. Use `/history` to fill gaps.
- The optional `station=ID` (percent-encoded, at most once; a repeated or any other parameter is `400`) shows only that station's events; an id that does not exist is fine and matches nothing. Nothing is replayed: use `/history` for the past.

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
- A client that reads too slowly loses events and gets `event: note` with `{"type":"note","text":"N events were dropped"}`; the stream continues.
- At most 16 streams at a time; the 17th gets `503` with a JSON error. The slot is freed when the client disconnects, when its socket stops accepting writes for 30 s or fails (TCP keepalive: first probe after 30 s idle, then every 10 s, 3 probes, where the platform allows it), or when the stream reaches its maximum age of 1 hour: the last event is then `event: note` with `{"type":"note","text":"the stream reached its maximum age; reconnect to continue"}` and the stream ends. A client that is connected but never reads is only cut by that age limit, since its kernel keeps acknowledging.
- A lag note also resets the per-stream state view: the next `state` event is complete, not a diff against events the client missed.
- Streams end when the server stops, after the closing events of the shutdown (`disconnected` with reason `shutdown`) were sent; the API stops after the CSMS. The token and `Origin` rules apply as for every route (`curl -N -H "Authorization: Bearer $TOKEN" ...`).

## Rules and faults

Same engine features as the script steps `respond` and `inject_fault` ([csms.md](csms.md#response-rules-and-fault-injection)); they apply to every station.

```text
$ curl -s -X PUT http://127.0.0.1:9100/v1/rules/Heartbeat -d '{"currentTime":"2030-01-01T00:00:00Z"}'
{"action":"Heartbeat","set":true}
$ curl -s http://127.0.0.1:9100/v1/rules
{"rules":["Heartbeat"]}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/rules/Heartbeat
{"action":"Heartbeat","cleared":true}

$ curl -s -X POST http://127.0.0.1:9100/v1/faults -d '{"action":"Heartbeat","kind":"call_error","times":2}'
{"injected":true,"remaining":2}
$ curl -s http://127.0.0.1:9100/v1/faults
{"remaining":2}
$ curl -s -X DELETE http://127.0.0.1:9100/v1/faults
```

- `PUT /v1/rules/{action}`: `{action}` must be an action a charge point can send in the protocol of the engine's action table, today `ocpp1.6` (otherwise `400` `unsupported_action`); the body must be a JSON object (the whole CALLRESULT payload, not checked against the schema).
- `POST /v1/faults` body: `{"action","kind","payload"?,"times"?}` with `kind` one of `call_error`, `silence`, `close`, `invalid_payload` (needs `payload`, sent as the CALLRESULT as given); `action` is 1 to 128 bytes without control characters (any other name is accepted, so faults can target actions the server does not know; `400` otherwise); `times` defaults to 1 and must be 1 to 1000000 (`400` otherwise); at most 1000 faults and 8 MiB of action and payload text are stored at once (`409` with a JSON error beyond that; `DELETE /v1/faults` frees them). Scripts apply the same checks and caps (an `inject_fault` step fails the script when the store is full); the terminal UI form is bounded by the same `times` limit. Faults are used in the order injected and take precedence over rules. Unknown fields are `400`.

## Errors and limits

Every error the API produces has the body `{"error":{"code":"<code>","message":"<text>"}}` and `Content-Type: application/json`. The `code` is stable (snake_case); the `message` is for people and may change. One exception: what hyper answers before the request reaches the router has an empty body and no `Content-Type`: `400` for a malformed request line or head (and for an invalid `Host`/header syntax hyper rejects), `431` for a head that is too large, and a connection closed without any answer when the head is not complete within the head timeout. Clients must not parse the body of those answers.

| Status | `code` | When |
| --- | --- | --- |
| 400 | `bad_request` | Invalid JSON or fields, bad `timeout_ms`, bad `limit`, unknown or repeated query parameter, unsafe station id, more than one `Host` header, bad fault, a rule body that is not an object |
| 400 | `unsupported_action` | `PUT`/`DELETE /v1/rules/{action}` with an action that is not a known charge-point action for the protocol (`ocpp1.6` today) |
| 401 | `unauthorized` | Token configured and missing or wrong |
| 403 | `forbidden` | No token configured and the request has an `Origin` header, `Sec-Fetch-Site: cross-site` or a non-loopback `Host` |
| 404 | `not_found` | Unknown route |
| 404 | `unknown_station` | The station id was never seen (or was forgotten) |
| 405 | `method_not_allowed` | Wrong method for the path |
| 408 | `request_timeout` | Request body not complete 10 s after the head |
| 409 | `station_not_connected` | A call to a station that is known but not connected |
| 409 | `conflict` | Forgetting a connected station; fault store full |
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
