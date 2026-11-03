# OCPP 1.6J JSON schemas

The first 56 files (`<Action>.json` for the CALL payload, `<Action>Response.json` for the
CALLRESULT payload) are the official OCPP 1.6J JSON schemas published by the
[Open Charge Alliance](https://openchargealliance.org/my-oca/ocpp/), copied unmodified.
They are embedded into `kirby-ocpp-core` with `include_str!` (see `src/ocpp/schema.rs`).

The OCPP 1.6J specification package references Creative Commons Attribution-NoDerivatives 4.0
International (https://creativecommons.org/licenses/by-nd/4.0/legalcode). The files are included
for conformance validation only and are not owned by this project; the project license applies to
this project's own source code, not to Open Charge Alliance materials. The Open Charge Alliance
does not sponsor or endorse this project. See `crates/kirby-ocpp/THIRD_PARTY_NOTICES.md`.

## Security Whitepaper schemas (22 files)

`CertificateSigned`, `DeleteCertificate`, `ExtendedTriggerMessage`, `GetInstalledCertificateIds`,
`GetLog`, `InstallCertificate`, `LogStatusNotification`, `SecurityEventNotification`,
`SignCertificate`, `SignedFirmwareStatusNotification` and `SignedUpdateFirmware` (each as
`<Action>.json` and `<Action>Response.json`) belong to the OCPP 1.6 Security Whitepaper
("Improved security for OCPP 1.6-J", edition 3). They are the official OCA files, copied unmodified
from the `schema/` folder of the whitepaper edition 3.2 package as mirrored in
`juherr/awesome-ev-charging` (`ocpp/Whitepapers/OCPP-1.6-security-whitepaper-edition-3-2/schema/`);
none of them is hand-authored. Differences from the core files: they declare JSON Schema draft-06
(`$schema`, `$id` `urn:OCPP:Cp:1.6:2020:3:...`) and use `definitions` / `$ref`. The registry
strips the root `$id` as it strips `id` and compiles them as draft-04, which gives the same
verdicts because they only use keywords the two drafts share. The files were not compared with the
OCA download itself (not reachable here), only with the whitepaper text and the field/enum lists of
the task.
