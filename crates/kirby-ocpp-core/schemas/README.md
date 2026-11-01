# OCPP 1.6J JSON schemas

These 56 files (`<Action>.json` for the CALL payload, `<Action>Response.json` for the
CALLRESULT payload) are the official OCPP 1.6J JSON schemas published by the
[Open Charge Alliance](https://openchargealliance.org/my-oca/ocpp/), copied unmodified.
They are embedded into `kirby-ocpp-core` with `include_str!` (see `src/ocpp/schema.rs`).

The OCPP 1.6J specification package references Creative Commons Attribution-NoDerivatives 4.0
International (https://creativecommons.org/licenses/by-nd/4.0/legalcode). The files are included
for conformance validation only and are not owned by this project; the project license applies to
this project's own source code, not to Open Charge Alliance materials. The Open Charge Alliance
does not sponsor or endorse this project. See `THIRD_PARTY_NOTICES.md` at the repository root.
