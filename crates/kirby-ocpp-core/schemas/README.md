# OCPP 1.6J JSON schemas

Files: `<Action>.json` for the CALL payload, `<Action>Response.json` for the CALLRESULT payload.
They are embedded into `kirby-ocpp-core` with `include_str!` (see `src/ocpp/schema.rs`).

All 78 files are the official Open Charge Alliance (OCA) schemas, copied unmodified:

- 56 files: the OCPP 1.6J schemas from "OCPP 1.6 (all files & errata)".
- 22 files: the Security Whitepaper schemas (`CertificateSigned`, `DeleteCertificate`,
  `ExtendedTriggerMessage`, `GetInstalledCertificateIds`, `GetLog`, `InstallCertificate`,
  `LogStatusNotification`, `SecurityEventNotification`, `SignCertificate`,
  `SignedFirmwareStatusNotification`, `SignedUpdateFirmware`, each with its response) from
  "Improved security for OCPP 1.6-J". Editions 3 and 4 ship identical schema files.

Both packages are downloaded from https://openchargealliance.org/my-oca/ocpp/ and were
compared byte for byte with the files here.

Attribution: Copyright (c) 2010-2025 Open Charge Alliance (specification package) and
(c) 2022, 2026 Open Charge Alliance (Security Whitepaper editions 3 and 4). All rights reserved.
The OCA makes these documents available under the Creative Commons Attribution-NoDerivatives 4.0
International Public License (https://creativecommons.org/licenses/by-nd/4.0/legalcode). The
materials are provided as is, without warranties. They are not owned by this project; the project
licence applies to its own source code only. The Open Charge Alliance does not sponsor or endorse
this project. See also `THIRD_PARTY_NOTICES.md` in the `kirby-ocpp` and `kirby-csms` crates.

Technical note: the whitepaper files declare JSON Schema draft-06 (`$schema`, `$id`
`urn:OCPP:Cp:1.6:2020:3:...`) and use `definitions` / `$ref`. The registry strips the root `$id`
as it strips `id` and compiles them as draft-04, which gives the same verdicts because they only
use keywords the two drafts share.
