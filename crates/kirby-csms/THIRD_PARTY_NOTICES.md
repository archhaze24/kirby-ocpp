# Third-Party Notices

## OCPP 1.6J JSON schemas

`kirby-csms` bundles the OCPP 1.6J JSON schemas (`crates/kirby-ocpp-core/schemas`) and embeds them in the binary for local protocol validation.

The schemas are sourced from the official Open Charge Alliance OCPP 1.6 download package:

- https://openchargealliance.org/my-oca/ocpp/

The OCPP 1.6J specification package references Creative Commons Attribution-NoDerivatives 4.0 International:

- https://creativecommons.org/licenses/by-nd/4.0/legalcode

The bundled schemas are included, unmodified, for compatibility and conformance validation and are not owned by this project. The project license in `LICENSE` applies to this project's own source code, not to Open Charge Alliance materials. Open Charge Alliance does not sponsor or endorse this project.

## OCPP 1.6 Security Whitepaper JSON schemas

The 22 schema files of the OCPP 1.6 Security Whitepaper ("Improved security for OCPP 1.6-J", edition 3) messages (CertificateSigned, DeleteCertificate, ExtendedTriggerMessage, GetInstalledCertificateIds, GetLog, InstallCertificate, LogStatusNotification, SecurityEventNotification, SignCertificate, SignedFirmwareStatusNotification, SignedUpdateFirmware and their responses) are the official Open Charge Alliance files of whitepaper edition 3.2, bundled unmodified and embedded in the binary for local protocol validation. They were copied from a public mirror of the OCA package (`juherr/awesome-ev-charging`), not from the OCA download itself. They are not owned by this project, the same terms and the same disclaimer as for the OCPP 1.6J schemas above apply (Open Charge Alliance does not sponsor or endorse this project), and the OCA's own licence terms for the whitepaper take precedence:

- https://openchargealliance.org/my-oca/ocpp/

## Rust dependencies

`kirby-csms` is statically linked from open-source Rust crates (tokio, rustls, ring, x509-parser, rcgen, ratatui, crossterm, clap, serde and others), each under its own licence (MIT, Apache-2.0, ISC, BSD and similar permissive licences). Their names, versions and licences are recorded in `Cargo.lock` and each crate's own metadata; run `cargo install cargo-license && cargo license` in a source checkout, or inspect the dependency tree with `cargo tree`, for the exact list. Binary distributions must keep this notice together with the licence texts of those crates.
