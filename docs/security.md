# Security

Support for the OCPP 1.6 Security Whitepaper (edition 4) in `kirby-csms` and `kirby-ocpp`: connection security profiles, TLS and mutual TLS, certificate management, security events, log upload and signed firmware. Like the rest of the project these are test tools: the station simulates uploads and firmware, and the CSMS does not sign certificates.

Contents: [Profiles](#profiles) · [CSMS side](#csms-side) · [Emulator side](#emulator-side) · [Basic Auth and AuthorizationKey](#basic-auth-and-authorizationkey) · [TLS and mTLS how-to](#tls-and-mtls-how-to) · [Station certificate flow](#station-certificate-flow) · [Security events](#security-events) · [GetLog and signed firmware](#getlog-and-signed-firmware) · [Secrets storage](#secrets-storage) · [Redaction and --show-secrets](#redaction-and---show-secrets) · [Known deviations](#known-deviations) · [Related pages](#related-pages)

## Profiles

| Profile | Transport | Identity |
| --- | --- | --- |
| 0 | `ws://`, or `wss://` with server authentication only | none |
| 1 | `ws://` or `wss://` | HTTP Basic Auth: user = station id, password = the station's `AuthorizationKey` |
| 2 | `wss://` | Basic Auth as in 1 |
| 3 | `wss://` with a client certificate | the certificate's Common Name equals the station id |

## CSMS side

`kirby-csms --security-profile N` (default 0). Flags and variables: [configuration.md](configuration.md).

| Profile | Required flags |
| --- | --- |
| 0 | none; `--auth-key*` and `--tls-client-ca` are rejected |
| 1 | at least one key: `--auth-key ID=KEY` (repeatable), `--auth-keys-file FILE` or `--default-auth-key KEY` |
| 2 | the keys of profile 1 plus `--tls-cert` and `--tls-key` |
| 3 | `--tls-cert`, `--tls-key` and `--tls-client-ca`; no `--auth-key*` |

Invalid combinations exit with code 2 before the server binds.

- Keys: `--auth-key` is visible in `ps` and shell history; prefer `KIRBY_CSMS_AUTH_KEY` (`ID=KEY;ID=KEY`, used only when no `--auth-key` is given) or `--auth-keys-file` (lines `ID=KEY`, `#` comments; keep it `0600`, its permissions are not checked). `--default-auth-key` covers stations without an entry. A station without any key is refused.
- Profiles 1 and 2: the user name must equal the station id from the URL and the password must equal that station's key, compared byte for byte (constant time).
- Profile 3: TLS with a client certificate that chains to `--tls-client-ca` and whose subject Common Name equals the station id. A station without a valid certificate fails the TLS handshake.
- Refusals answer HTTP 401 with a `WWW-Authenticate: Basic` challenge for credentials, 403 for certificate problems and `TLS required` on a plain socket. The body never says why. Each refusal prints an `auth_failed` event with a fixed reason (`missing or malformed credentials`, `no key configured for the station`, `invalid credentials`, `TLS required`, `client certificate required`, `client certificate does not match the station id`); keys are never printed. A failed TLS handshake under profile 3 is reported with `"station":null`.
- The TUI shows a `security profile N` chip and an `N auth failures` chip in the header.

What the CSMS does for the whitepaper messages:

- Answers `SignCertificate` with `Accepted` and the three notifications (`SecurityEventNotification`, `LogStatusNotification`, `SignedFirmwareStatusNotification`) with `{}`.
- Keeps in each station's state: the last 100 security events, the last log and signed firmware status, the last CSR, and the last `GetInstalledCertificateIds` answer per certificate type (at most 64 hashes).
- Has typed commands (TUI keys in [csms.md](csms.md#terminal-ui)) for `CertificateSigned` (`X`), `InstallCertificate` (`I`), `DeleteCertificate` (`d`), `GetInstalledCertificateIds` (`L`), `ExtendedTriggerMessage` (`E`), `GetLog` (`l`) and `SignedUpdateFirmware` (`S`). Over stdin, send them as raw `action` / `payload` commands.
- Does not sign CSRs, does not validate firmware signatures and does not enforce certificate checks beyond what rustls / webpki verify. To answer a CSR, sign it yourself (see [Station certificate flow](#station-certificate-flow)) and send `CertificateSigned`.

### Control API transport

The HTTP control API has its own listener and its own certificate, separate from the station listener and from the security profiles above. `--api-tls-cert` and `--api-tls-key` serve it over HTTPS (TLS 1.2 and 1.3); `--api-tls-client-ca` also requires a client certificate from every API client. A bearer token over plain HTTP is accepted only on a loopback address: a non-loopback `--api-listen` needs a token and TLS, and the process exits with code 2 before binding otherwise. Failed or silent TLS handshakes are dropped without a log line, limited to 5 s each and 64 at a time. The key file permissions are not checked; keep it `0600`. The full matrix and examples: [api.md](api.md#tls).

## Emulator side

`kirby-ocpp check` tests a connection with the TLS flags and the URL credentials only (certificate verification, `--tls-ca`, `--tls-cert`/`--tls-key`, `--tls-skip-verify`, a wildcard certificate is refused as above). It prints the subject, issuer and expiry of the server certificate and never the password of the URL; see [Connection check](emulator.md#connection-check).

There is no flag that sets the security profile (`--security-profile-fallback` only controls the fallback, below). The station follows the registry keys `SecurityProfile` and `AuthorizationKey`, which a CSMS sets with `ChangeConfiguration` as the whitepaper describes.

**Profile fallback (`A05.FR.08`, SHOULD).** After an accepted change of `SecurityProfile` the station remembers the profile that was in force (and the previous `AuthorizationKey` in the secrets file when the key changed in the meantime). If it then fails 3 times in a row to connect with the new profile before its first successful connection, it goes back to the previous profile (and key). It logs a warning, queues a `ReconfigurationOfSecurityParameters` security event whose `techInfo` names the fallback, and reconnects at once.

* Only evidence that the CSMS refused our credentials counts: an HTTP 401/403 upgrade answer or a TLS alert that rejects our client certificate.
* Never counted: server-certificate errors, generic TLS errors, closes, resets, timeouts, refused TCP and DNS errors.
* The reason is downgrade resistance. The CSMS may just be offline, and a man in the middle or a broken proxy can forge all of those failures. Counting them would force a downgrade and overwrite the `AuthorizationKey` with the previous one.
* Any failure that does not count restarts the count, so the 3 are really in a row.
* The previous key is restored only when the target profile is 1 or 2, and a state file with a previous profile above 3 is dropped with a warning.
* The window ends when the WebSocket upgrade succeeds (that is "connected" for `A05.FR.09`), whether or not the boot is accepted; the previous key is then deleted from the secrets file. A successful connection with the new profile ends the window for good (`A05.FR.09`: the station never falls back later).
* A key change at profile 0 keeps no previous key (nothing uses it), and a stored previous key without a pending switch at profile 0 is dropped at startup. With the fallback off, a pending record found in the state file is dropped.
* The fallback reconnects at once even with automatic reconnect disabled; with few attempts allowed the count carries over manual connects and restarts.
* The record is persisted (state file format 9), so a restart in the middle does not lose it.
* `--security-profile-fallback false` (`OCPP_SECURITY_PROFILE_FALLBACK`) turns the fallback off: the station then keeps retrying with the new profile.

- Profile 1 or 2: the station sends Basic Auth with user = charge point id and password = `AuthorizationKey`, unless the `--url` already carries a password (`wss://user:pass@host/path`; the userinfo becomes the `Authorization` header and never appears in the request line, `Host` or error messages).
- Profile 3: the station presents the stored charge point certificate (from `CertificateSigned`) as the TLS client identity, unless `--tls-cert` / `--tls-key` are given.
- `SecurityProfile` can only be raised. Exception: with `AllowSecurityProfileDowngrade` set to `true`, 3 may be lowered to 2. Lowering to 1 or 0 is never possible. Setting the current value is `Rejected` for 2 and 3 and a no-op `Accepted` for 0 and 1.
- Prerequisites when changing: 1 needs credentials (an `AuthorizationKey` or a URL password), 2 needs a `wss://` URL as well, 3 additionally a client certificate (`--tls-cert` or a stored one).
- After an accepted change of `SecurityProfile` or `AuthorizationKey`, and after `CertificateSigned` `Accepted`, the station sends the reply, disconnects and reconnects at once with the new parameters (even with auto-reconnect off).
- Transport settings are applied when a connection is opened, so other changes (for example `AllowCentralSystemTLSWildcards`) take effect at the next reconnect.
- Failed connection attempts raise security events, see [Security events](#security-events).

## Basic Auth and AuthorizationKey

- `AuthorizationKey` is a write-only configuration key: 16 to 40 printable ASCII characters. `GetConfiguration` lists it without a value.
- Whitepaper requirement A00.FR.205 says the key is hexadecimal (up to 40 hex digits) but never says the Basic Auth password is the decoded bytes. Both tools use the string as given, so non-hex printable keys also work. The CSMS compares the password byte for byte with the configured key; the emulator sends the key as the password. A peer that hex-decodes the key will not interoperate; the whitepaper leaves this open, so it is a known limitation, not a bug.
- Changing the key with `ChangeConfiguration` reconnects the station, so the CSMS-side key must be updated in step with it.
- The key comes from the station's secrets file after a restart, see [Secrets storage](#secrets-storage). With `--no-persist` it is not kept.

## TLS and mTLS how-to

Generate a test PKI with openssl. The station id here is `CP-1`; the server certificate needs a SAN matching the host the station connects to.

```sh
# CA
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
  -subj "/CN=Test CA" -keyout ca.key -out ca.pem

# Server certificate (SAN = localhost)
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj "/CN=localhost" -keyout server.key -out server.csr
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 \
  -extfile <(printf "subjectAltName=DNS:localhost,IP:127.0.0.1") -out server.pem

# Client certificate for profile 3: CN must equal the station id
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj "/CN=CP-1" -keyout cp1.key -out cp1.csr
openssl x509 -req -in cp1.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 \
  -extfile <(printf "extendedKeyUsage=clientAuth") -out cp1.pem
```

Server authentication only (profile 0), and Basic Auth over TLS (profile 2):

```sh
kirby-csms --tls-cert server.pem --tls-key server.key
kirby-ocpp --url wss://localhost:9000/ocpp --id CP-1 --tls-ca ca.pem

kirby-csms --security-profile 2 --tls-cert server.pem --tls-key server.key --auth-key CP-1=0123456789abcdef
kirby-ocpp --url wss://CP-1:0123456789abcdef@localhost:9000/ocpp --id CP-1 --tls-ca ca.pem
```

Mutual TLS (profile 3):

```sh
kirby-csms --security-profile 3 --tls-cert server.pem --tls-key server.key --tls-client-ca ca.pem
kirby-ocpp --url wss://localhost:9000/ocpp --id CP-1 --tls-ca ca.pem --tls-cert cp1.pem --tls-key cp1.key
```

Notes:

- `kirby-ocpp` verifies the server with the OS trust store unless `--tls-ca` is given. Use `--tls-server-name` when the URL host differs from the certificate name and `--tls-skip-verify` only against local self-signed servers. `--tls-cert` and `--tls-key` come as a pair.
- The CSMS reads PEM files: a certificate chain (leaf first) and one private key (PKCS#8, PKCS#1 or SEC1).
- The station refuses a Central System certificate with `*` in a SAN DNS name or in the subject Common Name unless `AllowCentralSystemTLSWildcards` is `true`. It cannot speak TLS below 1.2.
- Once a Central System root is installed with `InstallCertificate`, it becomes the TLS trust anchor in addition to `--tls-ca`, and the system roots are no longer used until that root is deleted.
- Emulator flags for the station side: [configuration.md](configuration.md).

## Station certificate flow

Charge point certificate (profile 3, or any time a CSMS wants a new one):

1. The station creates an ECDSA P-256 key and a CSR (`CN` = charge point id, `O` = `CpoName` or `Kirby`) and sends `SignCertificate`. Triggers: the TUI action on the Station tab (`C`, "request certificate"), `Command::RequestCertificate`, or the CSMS command `ExtendedTriggerMessage` with `SignChargePointCertificate`.
2. The CSMS tool records the CSR and answers `Accepted`. Sign it externally, for example: `openssl x509 -req -in csr.pem -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 -out cp1.pem`. Keep the CN equal to the station id if profile 3 will be used.
3. Send `CertificateSigned` (`X` in the TUI, PEM chain). The station answers `Accepted` only if a request is pending, the chain parses, fits `CertificateSignedMaxChainSize` (default 10 000 bytes), is linked (each certificate signed by the next), every certificate is currently valid and the leaf carries the pending public key. Otherwise `Rejected` plus an `InvalidChargePointCertificate` security event. Chain trust and the leaf Common Name are not verified.
4. After `Accepted` the station reconnects and, under profile 3, presents the new certificate.
5. Back-off: if no `CertificateSigned` arrives within `CertSigningWaitMinimum` seconds the same CSR is sent again, then after twice, four times, ... that wait, at most `CertSigningRepeatTimes` repeats, then the station gives up until a new request. The series is runtime state and a restart forgets it.
6. Renewal: the old chain and key are kept.
   - Under profile 3 the new certificate is tried first; a failed attempt switches to the other certificate, alternating without end (`A02.FR.28`), the client certificate and the Central System root each only on failures of their own kind (a server-certificate error never flips the client certificate): at once after a refusal (TLS client-certificate alert, HTTP 401/403 upgrade answer, connection closed or reset right after the TLS handshake), after 3 failed attempts in a row for anything else (timeout, refused TCP, DNS), so a short outage does not flip the choice constantly.
   - The first successful connection with the new certificate discards the other one. A success with the old one discards the new certificate (and queues `DiscardedRenewedClientCertificate`) only if the new one was refused at least once (a runtime flag); after a plain outage both are kept and the next attempt tries the new one again.

Root certificates:

- `InstallCertificate` (`I`): `CentralSystemRootCertificate` or `ManufacturerRootCertificate`, one PEM per call, at most 5500 characters, at most `CertificateStoreMaxLength` (default 8) in total. Only currently valid self-signed roots are accepted; anything else is `Rejected`, including a full store. Installing the same root twice is `Accepted` and changes nothing.
- `GetInstalledCertificateIds` (`L`): answers SHA-256 hash data (OCSP CertID rules: hash of the issuer name DER and of the issuer key bits, serial in lowercase hex).
- `DeleteCertificate` (`d`): accepts SHA-256/384/512 hash data. With `SecurityProfile` 2 or 3 the last Central System root cannot be deleted (`Failed`); this follows `M04.FR.06` and keeps a station from locking itself out of TLS.
- `--additional-root-certificate-check` (emulator, read-only key `AdditionalRootCertificateCheck`): a new Central System root must then be signed by the installed one; the old root stays as fallback until a TLS connection with the new one succeeds.
- Response rules on the emulator can target the four certificate commands (`reject` answers `Rejected`, `Failed`, `NotFound` and `Rejected` respectively).

## Security events

- The emulator sends `SecurityEventNotification` once registered, queues events while offline (at most 100, persisted, oldest dropped) and re-sends them in order after the next accepted boot. An error answer from the CSMS drops the event.
- Emitted automatically: `StartupOfTheDevice`, `ResetOrReboot`, `ReconfigurationOfSecurityParameters`, `InvalidChargePointCertificate`, `FailedToAuthenticateAtCentralSystem` (HTTP 401/403 on the upgrade), `InvalidCentralSystemCertificate`, `InvalidTLSVersion`, `InvalidTLSCipherSuite`, `DiscardedRenewedClientCertificate`, `InvalidFirmwareSignature`, `InvalidFirmwareSigningCertificate` and `FirmwareUpdated`. The four connection-failure events are queued once per series of failed attempts.
- `SettingSystemTime`, `MaintenanceLoginAccepted` and `MaintenanceLoginFailed` are never emitted automatically (no clock changes, no local maintenance interface). Any of the 21 whitepaper event types can be sent by hand: the `S` action on the emulator's Station tab (pick a type, optional techInfo) or `Command::SendSecurityEvent`.
- On the CSMS, `e` shows the last 100 events of the selected station; `techInfo` control characters are replaced.

## GetLog and signed firmware

Both are simulated on the emulator: nothing is uploaded or downloaded.

- `GetLog`: `remoteLocation` must be an absolute URL with a host and scheme ftp, ftps, http, https or sftp, else `Rejected`. `Accepted` carries a generated `filename`. The reply comes first, then `LogStatusNotification` `Uploading` and `Uploaded` (100 ms apart) with the `requestId`. A diagnostics-failure knob selects `UploadFailure`, with at most 5 retries. A `GetLog` while an upload runs is `AcceptedCanceled`.
- `SignedUpdateFirmware`: `location` must be an ftp/ftps/http/https/sftp URL, else `Rejected`; `signingCertificate` must parse and be currently valid, else `InvalidCertificate` (plus an `InvalidFirmwareSigningCertificate` event).
  - If a `ManufacturerRootCertificate` is installed, a certificate of the PEM must be that root or name it as issuer (name comparison only, no signature check).
  - The reply is followed by `SignedFirmwareStatusNotification`: `[DownloadScheduled]`, `Downloading`, `Downloaded`, `SignatureVerified`, `[InstallScheduled]`, `Installing`, `InstallRebooting`, `Installed`.
  - The firmware outcome knob selects failures (`DownloadFailed`, `InstallationFailed`, `InvalidSignature`, `InstallVerificationFailed`). The signature is never verified for real and the emulator does not restart on `InstallRebooting`.
- Response rules on the emulator can target `GetLog`, `ExtendedTriggerMessage` and `SignedUpdateFirmware` (`reject` answers `Rejected`).
- The CSMS shows the reported statuses per station (Station pane, `l` / `S` results).

## Secrets storage

The emulator keeps secrets out of its main state file `<state dir>/<id>.station.json`. They live next to it in `<id>.station.secrets.json`, mode `0600` on Unix:

- the private key of the charge point certificate, the key of the previous certificate and the key of a pending certificate request;
- the `AuthorizationKey`.

Certificates (public) are in the main file. The secrets file is never part of logs or snapshots (the snapshot shows `********`). `--no-persist` disables both loading and saving; `--state-dir` moves both files. The file format changes only together with a `STORE_VERSION` bump (9 now). Persistence details: [emulator.md](emulator.md).

The CSMS keeps nothing on disk. Its keys come from flags, the environment or `--auth-keys-file`; keep that file private.

## Redaction and --show-secrets

- The value of a `ChangeConfiguration` CALL for `AuthorizationKey` is replaced by `***` in recorded frames on both tools: TUI logs, `--json`, `--log-file`, history, and scenario / script matching. Reply payloads and CALLERROR details receive the same recursive redaction, including `GetConfiguration` results that echo the key. A `value` beside a case-insensitive `key = AuthorizationKey` is masked; unrelated values are preserved. The wire is unaffected. Other CALL actions (for example `DataTransfer`) are recorded as received.
- `--show-secrets` (`OCPP_SHOW_SECRETS` / `KIRBY_CSMS_SHOW_SECRETS`) keeps the real value in those places. Use it only for debugging; logs then contain the key.
- Schema errors for `AuthorizationKey` never quote the value, and `Debug` output of the configuration registry and the CSMS security policy masks keys.
- `kirby-csms --auth-key` is not redacted from the process list. Use the environment or a key file instead.

## Known deviations

Intentional or open points; see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md) for the complete list.

| Item | Behavior |
| --- | --- |
| A00.FR.205 hex key | The `AuthorizationKey` is used as given, not hex-decoded; non-hex printable keys are accepted |
| A05.FR.03 | An installed Central System root is not required for profile 2 or 3 (public CA setups are legitimate) |
| Leaf CN | No Common Name check on the `CertificateSigned` leaf (the whitepaper CN is the serial number, the CSR uses the station id) |
| A02.FR.28 and neighbours | Old/new certificate alternation: refusals switch at once, other failures after 3 in a row; a server that accepts both certificates keeps both until the new one is refused once; the refusal flag is not persisted, so after a restart a success with the old certificate keeps both |
| Store full | `InstallCertificate` answers `Rejected` (as OCPP 2.0.1), the whitepaper text is ambiguous |
| Last root | `DeleteCertificate` of the last Central System root is `Failed` only under profile 2 or 3 |
| Profile 2 | The station does not require TLS verification to be on (`--tls-skip-verify` still works) |
| CSMS keys | No eviction of station records (`forget()` only); default listener is unauthenticated (profile 0), so a peer can replace a live station by connecting with the same id; `--auth-key` is visible in `ps` |
| Firmware and logs | Simulated; signatures are not verified, no `RevokedCertificate`, a canceled signed update sends no further status |

## Related pages

- [csms.md](csms.md): the central system
- [emulator.md](emulator.md): the charge point
- [configuration.md](configuration.md): flags and environment variables
- [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md): full conformance notes
- [architecture.md](architecture.md): where the security code lives
