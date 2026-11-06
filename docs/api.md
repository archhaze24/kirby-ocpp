# HTTP control API

Drive a running `kirby-csms` from any language or CI without stdin plumbing. Local, JSON, plain HTTP, off by default.

Contents: [Start](#start) · [Auth model](#auth-model) · [Endpoints](#endpoints) · [Errors and limits](#errors-and-limits) · [Related pages](#related-pages)

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

## Errors and limits

| Status | When |
| --- | --- |
| 400 | Invalid JSON or fields, bad `timeout_ms`, bad `limit`, unsafe station id |
| 401 | Token configured and missing or wrong |
| 403 | No token configured and the request has an `Origin` header |
| 404 | Unknown route, unknown station, station not connected (calls) |
| 405 | Wrong method for the path |
| 409 | Forgetting a connected station |
| 413 | Request body over 1 MiB |
| 429 | Too many commands in flight |

## Related pages

[CSMS](csms.md) · [Configuration](configuration.md) · [Security](security.md) · [Architecture](architecture.md) · [Docs index](README.md)
