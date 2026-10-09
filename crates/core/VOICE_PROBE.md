# M1-V: opt-in live media fixture

This is a memory-only transport/audio experiment, **not production calling**.
The execution contract is `docs/goals/2026-10-09-live-voice-probe.md`.
No account, mailbox, Olm, schema, history or normal UniFFI API changes are made.
The pinned C carrier is reused without scheduler/congestion changes.

## Build and scope

Enable `dmsg-core/voice-probe` explicitly. The example requires that feature:

```sh
cargo test -p dmsg-core --features voice-probe --lib voice_probe::
cargo run -p dmsg-core --features voice-probe --example call_media_probe -- local
```

Use the repository's clean allowlist environment and a separate target directory
for diagnostic harnesses. `OPENSSL_SRC_PERL` may select the documented complete
Perl installation; do not forward credentials. Normal builds do not enable the
probe. Existing voice-note encoding, DVN validation and generated bindings remain
unchanged.

`local` uses two protected endpoints and an opaque loopback relay. One endpoint
receives caller-provided PCM; the other uses a synthetic fixture signal. It is
neither DNS evidence nor a listening/mouth-to-ear result. Synthetic capture keeps
continuous source positions through short callback delays, with a bounded 40ms
ring and age-based admission; it is not a timer that deletes microphone samples
on every missed tick.

## Immutable private fixtures

Create a fresh, owner-only directory and an owner-read-only public configuration.
The configuration has this shape (replace placeholders locally, never commit
actual deployment addresses):

```json
{
  "relay_addr": "127.0.0.1:<private-relay-port>",
  "domain": "m1v.fixture",
  "profile_ms": 40,
  "healthy_cycle_ms": 600,
  "capacity_bps": 50000,
  "carriers": [
    {"domain": "m1v.fixture", "resolvers": ["<numeric-resolver:port>"], "certificate_path": "<phone-private-certificate>"},
    {"domain": "m1v.fixture", "resolvers": ["<numeric-resolver:port>"], "certificate_path": "<host-private-certificate>"}
  ]
}
```

`healthy_cycle_ms` is the frozen healthy commit-to-terminal-feedback cycle, not
an RTT/2 estimate. Calibration runs must identify unqualified starting values.
`capacity_bps` is the declared safe useful-tunnel budget (50,000–80,000 bit/s),
not a measurement produced by successful local TCP writes. An offered-rate
limit does not emulate the carrier's service rate. Direct-authoritative, controlled
stub, recursive DNS, and any application service model must be labelled separately.
For host-only DirectTCP fixtures, use `carriers: [null, null]` and do not label the
result DNS. Profiles 20/40/60ms are immutable per run; their codec caps are
50/100/150 bytes respectively, not the same cap at different packet rates.

The example commands are:

```text
call_media_probe create-fixtures DIR PUBLIC_CONFIG
call_media_probe relay DIR BIND
call_media_probe endpoint ENDPOINT_FIXTURE SECONDS
call_media_probe rotate-fixtures OLD_DIR NEW_DIR
```

The authority creates `endpoint-a.json`, `endpoint-b.json`, `relay.json` and
`relay.key` using exclusive creation, then mode0400. Existing files are never
overwritten. Reads require a regular mode0400 file owned by the process, reject
symlinks, bound the size and check inode/metadata after opening. Put endpoint
secrets only on their endpoint; the relay receives only `relay.json` and
`relay.key`, **never SRTP master keys**. Fixture secrets are not argv/env/log data.
Phone provisioning must write directly into the `.gate` UID's private directory;
do not stage secret fixtures on shared storage or in world-readable device tmp.

Fixture authority supplies trust/material for this experiment. This does not
prove production E2E call-key negotiation. `rotate-fixtures` retains Noise static
trust identities but creates a fresh generation, directional SRTP keys, SSRCs
and source counters. Retire old peers and relay, cancel/join all lane workers and
stop/join both isolated native carriers **before** replacement. A timed-out join
is failure, not permission to start a second native owner. Old server C contexts
may survive until their existing idle cleanup. This is not per-lane reset,
sibling-preservation or production reconnect acceptance.

## Packet/queue contract

- Live Opus1.6.1: mono16k, VOIP, 12kbit/s, VBR with real encoding-time cap100,
  40ms/640samples, encoder10/decoder7, WB maximum/AUTO, DTX and exact-duration PLC.
  NoLACE/Deep PLC are tested in the linked artifact. All legal DTX packets are
  sent initially; no custom VAD/CNG. RTP uses PT111 and a 48kHz timestamp clock.
- One ordered bidirectional media lane and one control lane per endpoint; each
  has its own Noise state. Private opcodes240–243 live only in this feature, not
  the production protocol. Fresh Noise/domain authentication is followed by exact
  generation/role/lane/profile binding.
- One mutable Noise owner performs incremental duplex IO. One waiting request
  and one committed ciphertext frame are bounded independently. An expired
  request is dropped before nonce advancement. Once committed, finish it in
  order or destroy the lane; never skip ciphertext and continue its Noise state.
- RTP/SRTP AES_CM_128_HMAC_SHA1_80 and whole-compound SRTCP, no shortened tags/MKI.
  The relay forwards opaque same-lane packets in surviving order, never decodes
  or gets media keys, and bounds its pending hop and plaintext age.
- Private `M1V-RTCP-1`: SR-or-RR + SDES(eight printable CNAME octets) + APP32,
  every200ms, missed report ticks coalesced. No reduced-size/SDP/AVPF/general RTCP
  interoperability claim. APP reports authenticated terminal RTP frontier,
  retired playout cursor, late/PLC counters and local mute/DTX snapshots.
  The cursor is not a hardware ACK; mute snapshots do not classify lost peer audio.
- Source ledger records only committed index/framed bytes/time. Only validated
  terminal peer progress releases its ordered prefix; local writes and playout
  do not. A tail drop stays outstanding. Window/age/control expiry ends the
  generation rather than retaining or replaying seconds of speech.
- Fixed80ms jitter baseline; authenticated late packets are discarded without
  decoder rewind. Encoded future audio is bounded to200ms and one decoded PCM
  frame. The measured Android sink queue advances the decode/write deadline;
  it must not overwrite still-waiting native PCM.

At40ms the media framing overhead is44B (RTP12, SRTP10, application4,
Noise16, length2). Full protected SR-compound feedback is152 framed bytes,
RR132. Continuous mean media plus SR feedback is26.88kbit/s; the codec-cap
envelope is34.88kbit/s per uplink. No guaranteed TEXT reservation is implemented.
DTX, actual VBR rates, handshake bursts and competing traffic are measured.
Do not subtract outer DNS/IP overhead again from useful in-tunnel throughput.

## Android evidence boundaries

Only the debug `.gate` visible Activity owns this probe; it must have explicit
microphone permission and no running gate DNS service. Ten-ms JNI transfers use
independent capture/render workers, no store/Olm/network wait on audio calls.
Actual `AudioRecord`/`AudioTrack`, effects, routes, focus, sample/timestamp progress,
queue depth and cleanup are observed. `setPlaybackRate` is a measured platform
actuator, not an automatic remote-clock estimator or a custom resampler.

Instrumentation selects one exact method in `CallMediaProbeGatesTest`:

- `packagedLiveCodecActivationOnlyInGatePackage`
- `foregroundMicrophoneProtectedLoopbackAndCleanupOnlyInGatePackage`
- `foregroundMicrophoneProtectedDnsAndCleanupOnlyInGatePackage`

The last method requires an explicit private `probe_fixture_path` and
`probe_duration_seconds`6–120. Missing fixtures fail, never pass as a skip.
Install only the APKs built with `-PgateInstall=true`; never reset/install Main.
Permission grant precedes instrumentation; revocation is separate because it can
kill the runner UID. Lifecycle pause ends audio and joins its native owner.

Counter success proves protected delivery and local audio progress, **not**
conversational quality. Report each direction and source-active missing audio,
including pre-send drops, separately from startup/intentional teardown/outages.
Actual two-phone DNS and common-clock acoustic measurements remain mandatory for
physical acceptance; one phone + synthetic host, DirectTCP or RTT/2 cannot replace
them. Explicit near-boundary tests cover RTP16-bit sequence and32-bit timestamp
rollover;45minutes alone does not cover timestamp rollover or guarantee sequence
wrap with DTX.
