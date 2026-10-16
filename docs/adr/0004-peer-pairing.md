# ADR 0004: Peer pairing over QUIC with pinned fingerprints

Status: Accepted

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
  time, so removing a pin applies to the next handshake; a connection
  already open survives it.
- TLS is locked to version 1.3 and the suite list is filtered to the
  TLS 1.3 AEAD suites, so no TLS 1.2 or non-AEAD suite can be offered.
- SAS: six decimal digits from a commit-then-reveal exchange inside
  the TLS session. The dialer sends a hash commitment to a random
  nonce, the listener answers with its own nonce, the dialer reveals
  its nonce and the listener checks it against the commitment. The
  code hashes the TLS keying-material exporter output (RFC 5705, label
  `foldover-sas-v1`), both nonces and both device fingerprints, so it
  binds to this session and to the two identities.
- Why the commitment: an exporter-only code can be steered. A relay
  that is the TLS server on one leg picks its random after seeing the
  client's, so it can finish the other leg first and then retry
  candidate handshakes offline (about 500,000 signatures, seconds on a
  multicore machine) until both legs show the same six digits. With
  the commitment, each leg's code mixes a nonce the relay had not seen
  when it fixed its own input, as in Bluetooth numeric comparison.
- Tests: (1) honest pairing yields the same SAS on both sides and both
  pin each other; (2) a pinned reconnect carries data; (3) an unknown
  device is rejected in both directions; (4) a relay that terminates
  both legs with its own identity makes the two SAS codes differ; (5)
  removing a pin makes the next connection fail; (6) only TLS 1.3 AEAD
  suites are offered, the TLS 1.3 verification path ran on both ends,
  and exporter output is session-unique; (7) a dialer that reveals a
  nonce other than the one it committed to makes the listener abort.
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
3. MITM detected: a relay terminating both legs shows two different
   6-digit codes, and it cannot steer them to match: a match is chance
   only, 1 in 1,000,000 per pairing attempt. The harness shows the
   difference for a relay that runs the exchange honestly (test 4) and
   that a relay revealing a different nonce is caught (test 7); that no
   strategy beats chance is argued in Method, not measured.
4. One-step revocation: removing a pin makes the very next connection
   attempt fail.
5. Throughput for 1 GiB loopback on one and four streams is recorded
   for later budgets; there is no threshold.

## Results

First CI run of spike-s9
([run 38951986555](https://github.com/L-K-M/Foldover/actions/runs/38951986555)),
GitHub `ubuntu-24.04` runner, release build.

| Metric | Result |
| --- | --- |
| honest_pairing_shows_matching_sas_and_pins | ok |
| pinned_reconnect_succeeds | ok |
| unknown_device_rejected_in_both_directions | ok |
| mitm_relay_makes_sas_differ | ok |
| revocation_breaks_next_connection | ok |
| negotiated_tls13_with_aead_cipher | ok |
| cheating_dialer_fails_commitment_check | ok |
| Bench 1 stream, 1 GiB loopback (MiB/s) | 455.0 |
| Bench 4 streams, 1 GiB loopback (MiB/s) | 528.9 |

Criteria 1 to 4 pass. Loopback leaves the network out, so the
throughput is an upper bound on what encryption and QUIC framing
allow, not a LAN figure.

## Decision

Accepted: P5 builds peer mode on quinn and rustls (ring), TLS 1.3
only, with self-signed Ed25519 device certificates, the certificate
fingerprint as device ID, and a trust store read at every handshake.
The six-digit pairing code comes from the commit-then-reveal exchange,
never from the exporter or the fingerprints alone. Revoking a device
also closes its open connections. The TOFU fallback is not needed.

## Consequences

- All criteria pass: PLAN.md §4.11 keeps quinn + rustls, the
  fingerprint device ID and pinned fingerprints as the P5 design, and
  states the SAS precisely: derived from the session and a
  commit-then-reveal nonce exchange, not from the fingerprints alone.
  Revoking a device also closes its open connections. The recorded
  throughput feeds the P5 transfer budget.
- Six digits are judged too weak even at chance level (1 in 1,000,000
  per attempt; repeated failed pairings should lock the window):
  lengthen the code in §4.11.
- Unpaired rejection or revocation fails: fall back to the §8 S9
  alternative, TOFU with fingerprint confirmation, and re-open the
  inbound policy ("ask, allow paired, allow list") before P5.
- Throughput is unusable for sync budgets: revisit §4.11 transport
  options (stream limits, congestion control, or a non-QUIC channel)
  before P5.
