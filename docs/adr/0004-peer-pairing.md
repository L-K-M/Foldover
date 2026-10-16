# ADR 0004: Peer pairing over QUIC with pinned fingerprints

Status: Proposed

## Context

[PLAN.md §4.11](../../PLAN.md#411-peer-protocol-mac-to-mac) plans
Mac-to-Mac sync over QUIC (quinn + rustls). Each device holds an
Ed25519 key in the Keychain; the device ID is its certificate
fingerprint. Pairing shows a 6-digit code on both Macs, the user
confirms, and both sides pin each other's key, with one-click revoke
afterwards. Risk S9 in [PLAN.md §8](../../PLAN.md#8-risks-and-spikes)
asks whether that construction really delivers "no plaintext on the
wire; unpaired peers get nothing; one-click revoke" before P5 builds
on it. The fallback is TOFU with fingerprint confirmation.

## Question

Does QUIC (quinn) + rustls with self-signed device certificates, a
short authentication string (SAS) at pairing time and pinned
fingerprints afterwards give the security PLAN.md §4.11 promises?

## Method

The harness is a small Rust library plus tests and a bench binary in
`spikes/s9-peer-pairing/` (quinn 0.11, rustls 0.23 with the ring
provider, rcgen, sha2, tokio; versions pinned exactly).

- Identity: a fresh Ed25519 key in a self-signed certificate; the
  device ID is SHA-256 of the certificate DER.
- Verifier policy is an enum. `Pairing` accepts any peer certificate
  and records its fingerprint; `Pinned` accepts only fingerprints in a
  trust store and rejects everything else. Both sides always present a
  certificate (mutual TLS). The verifier reads the store at verify
  time, so removing a pin applies to the next handshake.
- TLS is locked to version 1.3 and the suite list is filtered to the
  TLS 1.3 AEAD suites, so no TLS 1.2 or non-AEAD suite can be offered.
- SAS: six decimal digits from the TLS keying-material exporter
  (RFC 5705, label `foldover-sas-v1`) mixed with both device
  fingerprints in canonical order, so the code binds to this session
  and to the two identities.
- Tests: (1) honest pairing yields the same SAS on both sides and both
  pin each other; (2) a pinned reconnect carries data; (3) an unknown
  device is rejected in both directions; (4) a relay that terminates
  both legs with its own identity makes the two SAS codes differ; (5)
  removing a pin makes the next connection fail; (6) only TLS 1.3 AEAD
  suites are offered, the TLS 1.3 verification path ran on both ends,
  and exporter output is session-unique.
- Bench: bytes/s for 1 GiB over loopback on one stream and on four
  parallel streams.

Run it with `spikes/s9-peer-pairing/run.sh results.json` (bash,
builds, runs `cargo test --release` and the bench, writes JSON) or
through the `spike-s9` workflow
([.github/workflows/spike-s9.yml](../../.github/workflows/spike-s9.yml)),
which appends the summary and JSON to the step summary and uploads the
JSON as an artifact. The workflow fails only on harness breakage; a
missed criterion is recorded, not thrown.

## Pass criteria

From PLAN.md §8 S9, made precise:

1. No plaintext: the only offered protocol is TLS 1.3, every suite is
   AEAD, and every connection is mutually authenticated (criteria 2
   and 3 exercise both verifier roles).
2. Unpaired peers get nothing: a peer whose fingerprint is not pinned
   cannot complete a handshake in either direction.
3. MITM detected: a relay terminating both legs produces two different
   TLS sessions, so the two 6-digit codes differ (a match by chance has
   probability 1 in 1,000,000, inherent to six digits).
4. One-step revocation: removing a pin makes the very next connection
   attempt fail.
5. Throughput for 1 GiB loopback on one and four streams is recorded
   for later budgets; there is no threshold.

## Results

Pending the first CI run of spike-s9.

| Metric | Result |
| --- | --- |
| honest_pairing_shows_matching_sas_and_pins | |
| pinned_reconnect_succeeds | |
| unknown_device_rejected_in_both_directions | |
| mitm_relay_makes_sas_differ | |
| revocation_breaks_next_connection | |
| negotiated_tls13_with_aead_cipher | |
| Bench 1 stream, 1 GiB loopback (MiB/s) | |
| Bench 4 streams, 1 GiB loopback (MiB/s) | |

Local run on the development host (Linux, release build, 2026-10-16):
6 of 6 tests passed; 270.7 MiB/s on one stream and 244.8 MiB/s on four
streams. CI numbers supersede these.

## Decision

Pending.

## Consequences

- All criteria pass: PLAN.md §4.11 keeps quinn + rustls, the
  fingerprint device ID, the exporter-derived SAS and pinned
  fingerprints as the P5 design; the recorded throughput feeds the P5
  transfer budget.
- SAS mismatch detection is judged too weak (six digits give a 1 in
  1,000,000 chance that a relay shows matching codes): lengthen the
  code or add a transcript hash comparison in §4.11, moving away from
  the plain 6-digit pairing.
- Unpaired rejection or revocation fails: fall back to the §8 S9
  alternative, TOFU with fingerprint confirmation, and re-open the
  inbound policy ("ask, allow paired, allow list") before P5.
- Throughput is unusable for sync budgets: revisit §4.11 transport
  options (stream limits, congestion control, or a non-QUIC channel)
  before P5.
