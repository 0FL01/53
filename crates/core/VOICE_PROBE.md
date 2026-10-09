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

Each carrier optionally selects `"congestion_control": "dcubic"` or `"bbr"`;
omission preserves `dcubic`. This selects an existing pinned native configuration,
not a scheduler/algorithm modification. BBR is a direct-path diagnostic recommended
by the pinned usage documentation; a LAN result does not choose the recursive-path
configuration. Keep the choice fixed and record it per run.

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
`probe_duration_seconds`6–3600, allowing the planned45-minute gate. Missing fixtures
fail, never pass as a skip. Measurement starts only after authenticated peer
readiness; DNS mode also requires the native carrier flag, not just Activity start.
It also waits for the frozen recording-clock observation and actual encoder/decoder
progress. `call_probe_ready` retains the startup baseline; `call_probe_progress`
emits a current sanitized snapshot every10s without catch-up messages. The host
example similarly emits structured `probe_ready`/`probe_progress` records before
its final stop result, separating healthy peer operation from the retirement tail.
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

## Observed checkpoint, 2026-10-09

The authorized Moto/API35 passed actual packaged WB40 NoLACE/Deep PLC activation
and the foreground protected local pair. The latest8-second local run decoded201
packets with0 PLC/late/render drops/underruns; actual sink depth stayed≤640samples.
The platform16008Hz actuator was observed at16007.707Hz; teardown joined in5ms.

One Moto plus a synthetic Linux peer also traversed two real pinned C clients,
a source-scoped controlled LAN DNS forwarder, a private authoritative carrier and
the opaque relay. This is neither USB DirectTCP nor public recursive DNS evidence.
The declared50kbit/s admission input and600ms feedback cycle were **not measured
DNS capacity/calibration**. No common-clock mouth-to-ear or listening pass exists.

| Short20-second fixture | Authenticated incoming RTP | Late packets | PLC slots | Result |
|---|---:|---:|---:|---|
|40ms, fresh epoch with unchanged Noise trust identities |497 |192 |192 |Protected DNS/lifecycle PASS; poor delivery |
|20ms |845 at early stop |519 |524 |Bounded generation retired before completion |
|60ms |330 |172 |172 |Protected DNS/lifecycle PASS; poor delivery |
|40ms, injected20ms DNS response delay |496 |35 |36 |Protected DNS/lifecycle PASS; not quality acceptance |

These are packet/slot counters, **not source-active loss percentages**. The first
40ms run was materially better; the variation does not qualify any profile.
Unmodified LAN fixtures emitted hundreds of thousands of queries during short
scripts, including startup/tail. The injected-delay comparison also dropped16676
responses at its bounded256-entry fixture queue, so it is a labelled impairment,
not a clean attribution of improved delivery solely to pacing. No scheduler,
congestion control, jitter target or progress window was changed to hide failure.

Actual native stop/join was observed at1–8ms on the phone and25–42ms on the host;
fresh-generation setup succeeded and all owned listeners/phone fixtures were
removed. This does not prove prompt per-lane abort or sibling preservation.
The selected baseline remains an **unqualified engineering candidate**. The
second-phone limitation of this historical checkpoint was subsequently removed;
the continuation below supersedes that blocker, not the negative observations.
See the durable goal for remaining evidence and checkpoint artifact hashes.

## Two-physical-phone continuation

Moto/API35 and Pacman/API36 are explicitly authorized; only their `.gate` packages
are installed/tested, never Main or its identity. Deterministic source regressions
showed that full sink lead must not declare a missing packet lost early, and a
ready authenticated lane packet must be consumed before timer-first concealment.
Native PCM expires as a whole remaining frame at its immutable source-frame end;
partial JNI pulls do not move that end. Only fully elapsed source slots are skipped
after a renderer stall, with no baseline rebase or historical catch-up burst.
Fixed80ms jitter, queue caps, codec profile and crypto/admission checks are unchanged.

The26 focused tests include forced timer-first ordering, decode/tick latency within
an active frame, partial pulls and a four-second renderer stall. Both actual local
8-second gates pass the unchanged active zero-drop assertion: Moto decoded200,
Pacman199, with0 active PLC/late/capture drops/render drops/skips/expiry/underruns.
Measurement emits the authenticated-ready baseline so startup and retirement are
not silently included in, or removed from, active loss accounting.

Both physical phones then ran the40ms profile for20seconds through real C carriers,
Noise/SRTP/SRTCP and the private relay. The direct-authoritative LAN control bypassed
the Python DNS forwarder after its nonblocking send failed with EAGAIN; that is a
fixture infrastructure failure, not a phone or Slipstream quality verdict.

| Direct-authoritative physical endpoint | Incoming RTP | Decoded | Late / PLC | Native render expiry samples |
|---|---:|---:|---:|---:|
| Moto |498 |411 |87 /87 |160 |
| Pacman |500 |500 |0 /0 |480 |

All87 Moto late arrivals were after their nominal due, not the corrected early-PLC
race. Both authenticated topology/lifecycle gates passed and native owners joined;
render expiry is counted, not disabled. No common-clock acoustic latency, listening
pass, measured narrow DNS capacity or poll-query count is claimed for this bypass
run. The declared50kbit/s admission/F0=600ms remain uncalibrated fixture inputs.
Continue isolating the asymmetric transport/CPU timing before profile selection;
absence of an acoustic measurement does not stop available engineering diagnosis.

## Source-clock and admission continuation (Moto + host)

The user temporarily took Pacman back; do not run new commands/tests on it until
explicit return. Independent source corrections and Moto/host measurements continue.

- Android JNI carries the oldest batch position and a valid monotonic AudioRecord
  frame/time reference. Reading buffered microphone PCM does not give it a fresh
  capture time. Invalid, unknown, stale or regressing references remain counted
  losses; source positions advance rather than repeating encoder input.
- Absolute observed hardware age is separate from the40ms application backlog cap.
  A local physical run proved that an absolute40ms cutoff rejected all actual Moto
  microphone input (hardware age up to74.271ms). The initial20 valid full160-sample
  observations freeze a minimum observed age floor; those calibration samples are
  counted startup drops. Only additional age above that fixed floor plus native
  waiting consumes the40ms cap. Later delays cannot grow the floor. This observation
  is not claimed intrinsic or acoustic device latency; absolute age remains visible.
- Integer bit-nanosecond credits reserve the6.08kbit/s feedback share **inside**
  the same75%-capacity allowance and original burst. A reproduced20ms capped-media
  load starved control at200ms with the old greedy shared bucket. Media may wait/drop,
  but cannot consume the reserved feedback share. At declared50k the remaining media
  allowance is31.42kbit/s; no new total allowance or guaranteed TEXT share is added.
- One waiting codec packet owns the original capacity-one lane permit, not an extra
  queue. Admission precedes SRTP/Noise and respects both encode age and source age.
  Pacing uses an immutable absolute source grid, at most one admission per slot.
  A deterministic500-frame test reproduced45 deadline drops from relative pacing
  under only1ms service jitter; the absolute grid admits all500 in-budget frames.
  Delayed commits do not move the epoch or authorize replaying old source slots.

After these corrections,52 focused probe tests and the307-test workspace pass
(two pre-existing explicit native ignores unchanged). Linked arm64 and full Android
builds/JVM122 pass. The frozen-floor local Moto gate has actual encoder/decoder
progress with no active capture/render drop or PLC/late; calibration remains visible.

| Direct-authoritative Moto + synthetic host,20s, existing BBR | Phone decoded | Phone late / PLC | Phone render expiry | Source limitation |
|---|---:|---:|---:|---|
|20ms, declared50k |1007 |2 /2 |0 |Small capture-age/alignment losses; capped envelope may exceed allowance |
|40ms, declared50k |502 |0 /0 |0 |Only initial3200 calibration samples dropped; zero active rejection/underrun |
|60ms, declared50k |335 |0 /0 |0 |Outgoing capture drops41280/rejected batches39; do not promote from incoming alone |

These are packet counters, not source-active loss percentages. Host final counters
include the expected shutdown tail and are not the healthy-interval loss rate.
Declared50k/F0=600ms are still not measured useful DNS capacity or healthy calibration.
There is no observer/query-count claim in the direct bypass, no common-clock acoustic
latency/listening pass, and no replacement of two-physical acceptance with this host.
The40ms engineering candidate now enters a45-minute bounded-queue/drift measurement.

### Clock-pair and bounded host-sink correction

The first planned45-minute run was interrupted at roughly742seconds; it is **not**
a completed stability gate. Its host consumer discarded one160-sample batch per
10ms without reporting sink depth. A deterministic reproduction shows that the
last160samples of a valid40ms decoded frame can expire at its unchanged frame end.
The CLI now models a virtual nominal16k sink with at most640submitted samples,
absolute fractional-clock consumption and up to four bounded pulls. It reports
that virtual depth to the existing playout API. This is not hardware/acoustic
playback, and the native queue/deadline has not been enlarged.

JNI brackets Java `System.nanoTime()` with native observations and uses the
completed observation as the capture-age origin. Copying and real native waiting
still consume the application budget; the observation bracket is reported because
this pairing can underestimate its tail. The frozen initial floor is unchanged.

These corrections have54 focused tests plus one CLI sink test; the workspace has
309 passing tests and the same two explicit native ignores. Native arm64 and full
Android/JVM122 builds pass. Actual local Moto input still exposes small capture-age
loss rather than hiding it; no absolute latency is inferred from the floor.

A fresh120s direct-authoritative Moto/host BBR run passed the authenticated topology
and cleanup gate: phone decoded2997, late6/PLC6, future rejection0, render expiry0,
underrun0. Capture drops4480 include3200initial calibration and1280additional
samples; hardware age maximum86954us/frozen floor46384us are observable. The host
healthy122s snapshot had late22/PLC24 and **zero** render expiry; final post-phone
shutdown counters are separate. These are packet/fixture counters, not source-active
quality percentages, acoustic latency or measured DNS capacity. The subsequent
45-minute records are described below, including their negative source counters.

### Opt-in synthetic useful-byte service

The endpoint's75% **offered admission** is not a50/80kbit/s service model. The
diagnostic relay now accepts an optional owner-read-only fixture configuration:

```text
call_media_probe relay FIXTURE_DIR LOOPBACK_BIND SYNTHETIC_SERVICE_CONFIG
```

For example, the nonsecret JSON configuration may be:

```json
{"baseline_bps":50000,"collapse":{"start_ms":10000,"duration_ms":300,"bps":20000}}
```

Baseline must be exactly50000 or80000; optional collapse is0..baseline for1..5000ms,
starting no later than3600000ms. Unknown fields and invalid bounds are rejected.
Omitting the optional argument preserves the original relay behavior.

Two independent source-role meters share each role's media/control allowance.
They start with zero credit at one common monotonic epoch **after all four lanes
authenticate and bind**. Setup bytes are excluded. The meter charges actual
nonblocking reads of the full prefix/Noise/application/SRTP-or-SRTCP bytes, not
codec payload alone. Burst is one maximum frame:152B for20/40ms and194B for60ms.
Idle credit stays bounded; both collapse boundaries discard unused credit. There
is no separate control allowance, outer-DNS/IP shaper, added packet queue or
native scheduler change. Partial frame offsets and Noise counters survive
service waits; retirement destroys partial records without refund or replay.

`probe_service_progress`/`probe_service_final` JSON explicitly labels this
`synthetic-framed-application-service`. Per-role read/authenticated/live-partial/
retired-partial bytes and credit wait are observable. Wait is summed over the two
lanes and includes reactor scheduling, not idle socket time. This is a synthetic
application-byte ceiling **after DNS transit**; runtime/carrier service can be
lower. It does not measure actual DNS safe capacity or acoustic latency.

The61 focused tests include exact296B combined media+feedback service:47.36ms at
50k and29.6ms at80k, shared per-role credit, zero-rate restoration, partial-frame
cancellation and following Noise counters. Workspace316 tests and the separate
CLI sink test pass, with the two unchanged explicit native ignores. Actual8s
**DirectTCP component-only** executions of the complete protected endpoints and
meter—not phone/DNS qualification—gave:

| Synthetic service | Decoded A/B | PLC A/B | Native render expiry A/B |
| --- | --- | --- | --- |
| 50k baseline | 194/194 | 0/0 | 0/0 |
| 80k baseline | 195/195 | 0/0 | 0/0 |
| 50k,300ms at20k | 192/189 | 2/5 | 0/0 |

Both role byte accounts matched authenticated frames with no retired partial
bytes; setup was excluded and aggregate service bounds held. These component
results do not replace applying the labelled model to the actual DNS fixture.

### Completed long records and bounded impairment

Two45-minute **Moto + synthetic Linux peer** direct-authoritative BBR records
completed the authenticated DNS/cleanup instrumentation. They used the published
pre-hardware-span sender grid; they are not latest two-Android, acoustic, continuous
speech, measured recursive capacity or source-active quality acceptance.

| 40ms run | Phone decoded / received RTP | Late / PLC | Native render expiry samples | Total captured samples dropped |
| --- | --- | --- | --- | --- |
| Default relay, no byte-service meter | 67137 /67502 | 363 /366 | 2720 | 549760 |
| Synthetic50k framed-byte service | 67483 /67504 | 18 /18 | 1120 | 21760 |

These are pre-teardown cumulative counters, including3200initial calibration
samples in capture drops. The host's last healthy snapshots were late373/PLC1220
and late501/PLC530 respectively, with native render expiry0 in both. Post-phone
shutdown progress expiry is excluded from the healthy interval. The first versus
last roughly five-minute source-drop deltas in the default run were33280/204800;
the50k-meter run had8960/640. Workloads/hardware clocks differ between runs, so
this is not causal proof that the meter fixes drift or latency.

The50k model finished at2701153ms: role read/authenticated/retired-partial bytes
were5478168/5478168/0 and9008966/9008869/97, with live partial0. The partial tail
remains charged. Declared50k/F0=600ms is not a measured DNS safe-capacity or feedback
calibration, and observer bypass provides no query-rate claim.

Actual DNS30s modelled50/80 baselines and a50k→20k300ms collapse also completed:
phone late/PLC19/19,1/1 and4/4, native render expiry0. They remain small counter
records, not active-speech percentages or normal/recovery latency percentiles.
Separate protected **DirectTCP component-only** zero-service tests produced:

| 50k service interruption | Decoded A/B | PLC A/B | Result |
| --- | --- | --- | --- |
| 0bps100ms | 191/192 | 3/2 | Bounded continuation |
| 0bps300ms | 180/181 | 14/13 | Bounded continuation |
| 0bps1000ms | 73/73 | 14/14 | Expected terminal progress expiry |
| Fresh generation after1000ms case | 194/194 | 0/0 | Same Noise trust pins, new media keys/SSRC/state |

The1000ms case charged consumed partial frames without refund; fresh generation
does not replay them. This is component retirement, not production reconnect or
an acoustic recovery-time guarantee.

### Hardware-clock-aware sender admission

The nominal16000 sender grid had a separate deterministic defect: valid punctual
capture at+100/+500ppm gradually waited past unchanged plaintext deadlines. A
45-minute simulated capture test reproduced first drops at490.020/98.020s for
20ms and490.060/98.060s for40ms; the60ms limits were390.070/78.070s. Slower capture
was already loss-free. This source proof does not attribute the physical records
above to that defect.

Admission now projects the immutable source epoch using integral hardware
frame/nanosecond spans from validated AudioRecord observations. Invalid references
cannot move those anchors; transport/encoding/Noise receipt time does not estimate
the source frequency. Consumed source-slot ordinals remain monotonic through span
updates and delayed commits. Host synthetic input retains nominal timing.

All990000 modeled20/40/60ms frame attempts across±100/500ppm now admit without
deadline drops or cumulative ready waiting. Positive nominal source phase remains
observable, not rebased away. Byte allowances, fixed80 receiver, capture floor,
age bounds, queues, one pending lane reservation, feedback and crypto are unchanged.
This is sender correctness, **not** end-to-end skew compensation or a resampler.

Latest checks:64 focused probe tests, CLI sink test and319 workspace tests pass;
the same two explicit native ignores remain. Arm64/API26 and full Android/JVM122
builds pass. Actual local Moto8s gate has decoded210/PLC0/late0, no active capture
or render loss, and cleanup. A fresh60s actual-DNS synthetic50k run has decoded1501,
late1/PLC1, future rejection0, native render expiry0, and only3200calibration capture
drops; intentional teardown is scored separately. Absolute hardware age78737us,
frozen floor39503us and clock-observation uncertainty4198us remain visible. No
acoustic/source-active quality or latest two-phone PASS is inferred.
