# OCPP 2.0.1 JSON schemas

Files: `<Action>Request.json` for the CALL payload, `<Action>Response.json` for the CALLRESULT
payload. They are embedded into `kirby-ocpp-core` with `include_str!` (see
`src/ocpp/schema_v201.rs`) and compiled as JSON Schema draft-06 by `src/ocpp/schema.rs`.

All 128 files (64 request/response pairs) are the official Open Charge Alliance (OCA) schemas
of "OCPP 2.0.1 (all files)", the `OCPP-2.0.1_part3_JSON_schemas` directory dated 2020-03-31,
copied unmodified. The package is downloaded from https://openchargealliance.org/my-oca/ocpp/
and was compared byte for byte with the files here.

Attribution: Copyright (c) 2010-2025 Open Charge Alliance. All rights reserved. The OCA makes
these documents available under the Creative Commons Attribution-NoDerivatives 4.0
International Public License (https://creativecommons.org/licenses/by-nd/4.0/legalcode). The
materials are provided as is, without warranties. They are not owned by this project; the project
licence applies to its own source code only. The Open Charge Alliance does not sponsor or endorse
this project. See also `THIRD_PARTY_NOTICES.md` in the `kirby-ocpp` and `kirby-csms` crates.

Technical note: every file declares `"$schema": "http://json-schema.org/draft-06/schema#"`, a
`$id` of the form `urn:OCPP:Cp:2:2020:3:<Action>Request` and uses only local
`#/definitions/...` references. The registry strips the root `$schema`, `id` and `$id` before
compiling and keeps nested keys such as property `id` untouched. The schemas do not encode every
requirement of the specification text (for example some integer ranges, identifier alphabets and
timestamp precision); those are checked outside the schema layer.
