# kirby-csms-core

OCPP 1.6J central system (CSMS) engine, no UI. It is the engine behind the [`kirby-csms`](https://crates.io/crates/kirby-csms) terminal tool.

- Multi-station WebSocket server: the station id is the last URL path segment, with a bounded per-station frame history.
- Per-station protocol read model (boot info, connectors, transactions with meter readings) and an event stream.
- Typed, schema-checked CSMS commands (`reset`, `remote_start_transaction`, ...) and raw `call`.
- Scripted replies and fault injection, to exercise how charge point firmware handles errors.
- Security profiles 1 to 3: Basic Auth, TLS, mTLS with the certificate CN as the station id.

```rust
use kirby_csms_core::{CsmsConfig, CsmsEvent, CsmsServer};

let csms = CsmsServer::start(CsmsConfig::new("0.0.0.0:9000".parse()?)).await?;
let mut events = csms.subscribe();
while let Ok(event) = events.recv().await {
    if let CsmsEvent::Connected { station } = event {
        println!("{station} connected");
    }
}
```

API documentation: <https://docs.rs/kirby-csms-core>. Project home and CLI usage: <https://github.com/archhaze24/kirby-ocpp>.

## Licence

MIT. The bundled OCPP JSON schemas belong to the Open Charge Alliance and are used under CC BY-ND 4.0, see the `kirby-ocpp-core` crate.
