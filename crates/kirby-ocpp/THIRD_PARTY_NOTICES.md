# Third-Party Notices

## OCPP JSON schemas (1.6J, 1.6 Security Whitepaper, 2.0.1)

`kirby-ocpp` bundles the JSON schemas of the OCPP 1.6J specification package (56 files, `crates/kirby-ocpp-core/schemas`), of the OCPP 1.6 Security Whitepaper "Improved security for OCPP 1.6-J" (22 files: CertificateSigned, DeleteCertificate, ExtendedTriggerMessage, GetInstalledCertificateIds, GetLog, InstallCertificate, LogStatusNotification, SecurityEventNotification, SignCertificate, SignedFirmwareStatusNotification, SignedUpdateFirmware and their responses) and of the OCPP 2.0.1 specification package (128 files, `crates/kirby-ocpp-core/schemas/ocpp201`). They are embedded in the binary for local protocol validation.

- Creator and copyright holder: Open Charge Alliance. Copyright (c) 2010-2025 Open Charge Alliance for the OCPP 1.6 and OCPP 2.0.1 specification packages, copyright (c) 2022 and 2026 Open Charge Alliance for Security Whitepaper editions 3 and 4. All rights reserved by the Open Charge Alliance.
- License: the Open Charge Alliance publishes these materials under the Creative Commons Attribution-NoDerivatives 4.0 International Public License (CC BY-ND 4.0): https://creativecommons.org/licenses/by-nd/4.0/legalcode
- Source: the official downloads at https://openchargealliance.org/my-oca/ocpp/ ("OCPP 1.6 (all files & errata)", "OCPP 2.0.1 (all files)", "OCPP 1.6 Security Whitepaper Edition 4" and the archived Edition 3). The bundled files are byte-for-byte identical to the schemas in those packages (the whitepaper schemas are identical in editions 3 and 4). They are included unmodified.
- Disclaimer: the materials are provided as is, without warranties of any kind, as stated in section 5 of the CC BY-ND 4.0 license. The Open Charge Alliance does not sponsor or endorse this project.

The project license in `LICENSE` applies to this project's own source code, not to Open Charge Alliance materials.

## Rust dependencies

`kirby-ocpp` is statically linked from open-source Rust crates (tokio, rustls, ring, x509-parser, rcgen, ratatui, crossterm, clap, clap_complete, serde and others), each under its own license (MIT, Apache-2.0, ISC, BSD and similar permissive licenses). Their names, versions and licenses are recorded in `Cargo.lock` and each crate's own metadata; run `cargo install cargo-license && cargo license` in a source checkout, or inspect the dependency tree with `cargo tree`, for the exact list. Binary distributions must keep this notice together with the license texts of those crates.
