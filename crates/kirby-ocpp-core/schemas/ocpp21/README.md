# OCPP 2.1 JSON schemas

Files: `<Action>Request.json` for the CALL payload, `<Action>Response.json` for the CALLRESULT
payload, and `NotifyPeriodicEventStream.json` for the payload of the one SEND message. They are
embedded into `kirby-ocpp-core` with `include_str!` (see `src/ocpp/schema_v21.rs`) and compiled
as JSON Schema draft-06 by `src/ocpp/schema.rs`.

All 181 files (90 request/response pairs plus the SEND payload) are the official Open Charge
Alliance (OCA) schemas of "OCPP 2.1 (all files)", edition 2, the `OCPP-2.1_part3_JSON_schemas`
directory dated 2025-01-23, copied unmodified. The package is downloaded from
https://openchargealliance.org/my-oca/ocpp/ and was compared byte for byte with the files here.
The `comment` keyword inside each file reads "OCPP 2.1 Edition 1"; the text is part of the
official files and was left as is.

Attribution: Copyright (c) 2010-2025 Open Charge Alliance. All rights reserved. The OCA makes
these documents available under the Creative Commons Attribution-NoDerivatives 4.0
International Public License (https://creativecommons.org/licenses/by-nd/4.0/legalcode). The
materials are provided as is, without warranties. They are not owned by this project; the project
licence applies to its own source code only. The Open Charge Alliance does not sponsor or endorse
this project. See also `THIRD_PARTY_NOTICES.md` in the `kirby-ocpp` and `kirby-csms` crates.

Technical note: every file declares `"$schema": "http://json-schema.org/draft-06/schema#"`, a
`$id` of the form `urn:OCPP:Cp:2:2025:1:<Name>` and uses only local `#/definitions/...`
references. `NotifyPeriodicEventStream` is a SEND message (an unconfirmed, one-way frame), so
its file has no `Request` or `Response` suffix and there is no response schema. The registry
strips the root `$schema`, `id` and `$id` before compiling and keeps nested keys such as
property `id` untouched. The schemas do not encode every requirement of the specification text
(for example some identifier alphabets and timestamp precision); those are checked outside the
schema layer.
