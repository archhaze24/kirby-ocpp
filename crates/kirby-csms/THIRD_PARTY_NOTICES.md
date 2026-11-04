# Third-Party Notices

## OCPP 1.6J JSON schemas and Security Whitepaper schemas

`kirby-csms` bundles the JSON schemas of the OCPP 1.6J specification package (56 files, `crates/kirby-ocpp-core/schemas`) and of the OCPP 1.6 Security Whitepaper "Improved security for OCPP 1.6-J" (22 files: CertificateSigned, DeleteCertificate, ExtendedTriggerMessage, GetInstalledCertificateIds, GetLog, InstallCertificate, LogStatusNotification, SecurityEventNotification, SignCertificate, SignedFirmwareStatusNotification, SignedUpdateFirmware and their responses). They are embedded in the binary for local protocol validation.

- Creator and copyright holder: Open Charge Alliance. Copyright (c) 2010-2025 Open Charge Alliance for the OCPP 1.6 specification package, copyright (c) 2022 and 2026 Open Charge Alliance for Security Whitepaper editions 3 and 4. All rights reserved by the Open Charge Alliance.
- Licence: the Open Charge Alliance publishes these materials under the Creative Commons Attribution-NoDerivatives 4.0 International Public License (CC BY-ND 4.0): https://creativecommons.org/licenses/by-nd/4.0/legalcode
- Source: the official downloads at https://openchargealliance.org/my-oca/ocpp/ ("OCPP 1.6 (all files & errata)", "OCPP 1.6 Security Whitepaper Edition 4" and the archived Edition 3). The bundled files are byte-for-byte identical to the schemas in those packages (the whitepaper schemas are identical in editions 3 and 4). They are included unmodified.
- Disclaimer: the materials are provided as is, without warranties of any kind, as stated in section 5 of the CC BY-ND 4.0 licence. The Open Charge Alliance does not sponsor or endorse this project.

The project licence in `LICENSE` applies to this project's own source code, not to Open Charge Alliance materials.

## Rust dependencies

`kirby-csms` is statically linked from open-source Rust crates (tokio, rustls, ring, x509-parser, rcgen, ratatui, crossterm, clap, serde and others), each under its own licence (MIT, Apache-2.0, ISC, BSD and similar permissive licences). Their names, versions and licences are recorded in `Cargo.lock` and each crate's own metadata; run `cargo install cargo-license && cargo license` in a source checkout, or inspect the dependency tree with `cargo tree`, for the exact list. Binary distributions must keep this notice together with the licence texts of those crates.
