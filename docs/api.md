# HTTP control API

Drive a running `kirby-csms` from any language or CI without stdin plumbing. Local, JSON, plain HTTP, off by default.

Contents: [Start](#start) · [Auth model](#auth-model) · [Endpoints](#endpoints) · [Event stream](#event-stream) · [Rules and faults](#rules-and-faults) · [Errors and limits](#errors-and-limits) · [Related pages](#related-pages)

## Start

```
kirby-csms --listen 127.0.0.1:9000 --api-listen 127.0.0.1:9100
api listening on http://127.0.0.1:9100
```

The `api listening` line comes right after the `listening` line (with `--json`: `{"type":"api_listening","addr":"127.0.0.1:9100","url":"http://127.0.0.1:9100"}`). Port `0` picks a free port. The API works in the TUI, headless and `--script` modes and stops (gracefully, waiting at most 3 s for open requests) when the server stops. Flags: [configuration.md](configuration.md#listener-and-security). The API uses only the same engine handle as the stdin commands; it adds no new capabilities to the engine.

## Auth model

- Without a token the API accepts only loopback addresses (`--api-listen 0.0.0.0:...` without a token exits with code 2). Any local process of any user can use it, and a request carrying an `Origin` header (a browser page, also via DNS rebinding) gets `403`.
- With `--api-token TOKEN` / `--api-token-file FILE` (env `KIRBY_CSMS_API_TOKEN`, `KIRBY_CSMS_API_TOKEN_FILE`) every request, whatever the path, needs `Authorization: Bearer TOKEN` (compared in constant time), otherwise `401` with `WWW-Authenticate: Bearer`. `--api-token` is visible in `ps`; prefer the environment variable or the file (read once, trailing newline dropped, must not be empty).
- There is no TLS and no per-user permission: the token is sent in clear text. Keep the API on loopback, or put it behind a reverse proxy or SSH tunnel that adds TLS. Everything the API can do (sending any CALL to any station, forgetting stations) is available to every holder of the token.

## Endpoints

JSON in and out, `Content-Type: application/json`. Errors have the body `{"error":"..."}`. Station ids in the path are percent-decoded; an id with control or bidi characters is `400`. Text from stations is escaped like the `--json` lines.

| Method and path | Meaning |
| --- | --- |
| `GET /v1/health` | `{"ok":true,"version":"...","stations":N}` |
| `GET /v1/stations` | `{"stations":[...]}`, the objects of the stdin `{"cmd":"stations"}` answer |
| `GET /v1/stations/{id}` | The station summary plus `state` (boot info, connectors, transactions, ...); `404` if unknown |
| `GET /v1/stations/{id}/history?limit=N` | `{"station":..,"frames":[...]}`, oldest first, only the newest `N` with `limit` (`0` gives none); frames are shaped like the `--json` frame lines; `AuthorizationKey` values are redacted unless `--show-secrets`; `404` if unknown |
| `POST /v1/stations/{id}/calls` | Sends a CALL; body `{"action":"Reset","payload":{...},"timeout_ms":5000}` (`payload` default `{}`, `timeout_ms` 1..=86400000, default `--call-timeout`). Answers with the stdin result object (`type`, `station`, `action`, `ok`, `result` or `error`/`code`/`details`) |
| `DELETE /v1/stations/{id}` | Forgets a disconnected station (`{"station":..,"forgotten":true}`); `409` while it is connected, `404` if unknown |
| `GET /v1/events?station=ID` | Server-Sent Events, see [Event stream](#event-stream) |
| `GET /v1/rules` | `{"rules":["Heartbeat",...]}`: actions that have a fixed reply, sorted |
| `PUT /v1/rules/{action}` | Sets the fixed CALLRESULT payload (body: a JSON object) for a charge-point action |
| `DELETE /v1/rules/{action}` | Removes the rule (`{"action":..,"cleared":true\|false}`); restores the default answer |
| `POST /v1/faults` | Injects a fault, see [Rules and faults](#rules-and-faults) |
| `GET /v1/faults` | `{"remaining":N}`: fault occurrences not triggered yet |
| `DELETE /v1/faults` | Clears all faults (`{"remaining":0}`) |

Status of a call: `200` also when the station answered with a CALLERROR or the call timed out (`"ok":false` in the body); `404` when the station is unknown or not connected (the body is the result object with `error`); `400` for a bad body; `413` above 1 MiB; `429` when 256 commands (stdin and API together) are already in flight.

```
$ curl -s http://127.0.0.1:9100/v1/stations
{"stations":[{"id":"CP-1","connected":true,"vendor":"Kirby","model":"Emu","connectors":{"1":"Available"},"active_transactions":[]}]}

$ curl -s -X POST http://127.0.0.1:9100/v1/stations/CP-1/calls \
    -H 'Authorization: Bearer '"$TOKEN" \
    -d '{"action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"}}'
{"type":"result","station":"CP-1","action":"ChangeConfiguration","ok":true,"result":{"status":"Accepted"}}

$ curl -s 'http://127.0.0.1:9100/v1/stations/CP-1/history?limit=5'
$ curl -s -X DELETE http://127.0.0.1:9100/v1/stations/CP-1
```

## Event stream

`GET /v1/events` is a Server-Sent Events stream of everything the server sees, from the moment of the request. Each SSE message has `event: <type>` and one `data:` line holding the same JSON object that `--json` prints for that event (types `connected`, `disconnected`, `frame`, `state`, `malformed`, `forgotten`, `auth_failed`; text is escaped the same way and `AuthorizationKey` values are redacted unless `--show-secrets`). The optional `station=ID` (percent-encoded) shows only that station's events; an id that does not exist is fine and matches nothing. Nothing is replayed: use `/history` for the past.

```
$ curl -N http://127.0.0.1:9100/v1/events?station=CP-1
event: connected
data: {"station":"CP-1","ts":"2030-01-01T10:00:00.000Z","type":"connected"}

event: frame
data: {"action":"Heartbeat","direction":"in","id":"...","kind":"call","payload":{},"station":"CP-1","ts":"...","type":"frame"}

: keep-alive
```

- A `: keep-alive` comment is sent every 15 s.
- A client that reads too slowly loses events and gets `event: note` with `{"type":"note","text":"N events were dropped"}`; the stream continues.
- At most 16 streams at a time; the 17th gets `503` with a JSON error. The slot is freed when the client disconnects.
- Streams end when the server stops. The token and `Origin` rules apply as for every route (`curl -N -H "Authorization: Bearer $TOKEN" ...`).

## Rules and faults

Same engine features as the script steps `respond` and `inject_fault` ([csms.md](csms.md#response-rules-and-fault-injection)); they apply to every station.

```
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

- `PUT /v1/rules/{action}`: `{action}` must be an action a charge point can send in OCPP 1.6 (otherwise `400`); the body must be a JSON object (the whole CALLRESULT payload, not checked against the schema).
- `POST /v1/faults` body: `{"action","kind","payload"?,"times"?}` with `kind` one of `call_error`, `silence`, `close`, `invalid_payload` (needs `payload`, sent as the CALLRESULT as given); `times` defaults to 1 and must be 1 or more. Faults are used in the order injected and take precedence over rules. Unknown fields are `400`.

## Errors and limits

| Status | When |
| --- | --- |
| 400 | Invalid JSON or fields, bad `timeout_ms`, bad `limit`, unknown query parameter, unsafe station id, unknown rule action, bad fault |
| 401 | Token configured and missing or wrong |
| 403 | No token configured and the request has an `Origin` header |
| 404 | Unknown route, unknown station, station not connected (calls) |
| 405 | Wrong method for the path |
| 409 | Forgetting a connected station |
| 413 | Request body over 1 MiB |
| 429 | Too many commands in flight |
| 503 | Already 16 event streams open |

## Related pages

[CSMS](csms.md) · [Configuration](configuration.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
