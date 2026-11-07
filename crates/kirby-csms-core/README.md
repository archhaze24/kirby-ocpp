# kirby-csms-core

OCPP 1.6J central system (CSMS) engine, no UI. It is the engine behind the [`kirby-csms`](https://crates.io/crates/kirby-csms) tool. It builds on `kirby-ocpp-core` (frames, schemas, WebSocket layer).

**Most users want the binary**, not this library:

```
cargo install kirby-csms    # or: npm install -g kirby-csms
```

## What it is

- Multi-station WebSocket server: the station id is the last URL path segment; bounded per-station frame history.
- Per-station protocol read model (`StationState`: boot info, connectors, transactions with meter readings) and an event stream (`CsmsEvent`).
- Typed, schema-checked CSMS commands (`reset`, `remote_start_transaction`, ...) plus raw `call`.
- Response rules and fault injection (`respond`, `inject`) to exercise charge point firmware.
- Security profiles 1 to 3: Basic Auth, TLS, mTLS with the certificate CN as the station id.

It is not a UI, not a CLI and not a billing or backend system.

## Entry points

`CsmsServer::start(CsmsConfig)` returns a `CsmsHandle`: `call`, `call_typed`, `state`, `stations`, `history`, `subscribe`, `shutdown`.

```rust,no_run
use kirby_csms_core::{CsmsConfig, CsmsError, CsmsEvent, CsmsServer};
use serde_json::json;

# async fn run() -> Result<(), CsmsError> {
let csms = CsmsServer::start(CsmsConfig::new("0.0.0.0:9000".parse().unwrap())).await?;
let mut events = csms.subscribe();
while let Ok(event) = events.recv().await {
    if let CsmsEvent::Connected { station } = event {
        let reply = csms
            .call(&station, "ChangeConfiguration", json!({ "key": "HeartbeatInterval", "value": "60" }))
            .await?;
        println!("{station}: {}", reply.to_json_string());
    }
}
csms.shutdown().await;
# Ok(())
# }
```

## Stability

Pre-1.0: the API may change between minor versions.

## Docs

- [Architecture](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md)
- [CSMS guide](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md), [security profiles](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/security.md)
- [HTTP control API](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/api.md) (of the `kirby-csms` binary)
- API reference: <https://docs.rs/kirby-csms-core>

## License

MIT. The bundled OCPP JSON schemas belong to the Open Charge Alliance and are used under CC BY-ND 4.0, see the `kirby-ocpp-core` crate.
