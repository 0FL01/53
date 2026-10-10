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

### Historical 80k long record and reproduced receive-clock limitation

The **e98dd3a** Moto + synthetic Linux peer run completed2700021ms of the
direct-authoritative DNS fixture with BBR and synthetic80k framed-ingress service.
Instrumentation and cleanup passed; phone NativeClient stop/join took2ms (probe3ms).
Its pre-teardown counters were decoded67382/RTP67506, late122/PLC122,
capture drops95360 (including3200calibration), native render expiry640 samples,
future rejection0 and7 AudioTrack underruns. These are counters, **not** active
speech loss or an acoustic-latency result. The host's last healthy snapshot was
late200/PLC344, capture drops25600 (unchanged from readiness) and render expiry0;
the expected post-phone progress expiry is not part of the healthy interval.

Final service accounting at2701321ms was read/authenticated6072748/6072748 and
9009587/9009587 bytes, live/retired partial0 for both roles. The model charges only
framed application ingress after DNS transit; it does not measure recursive DNS
capacity. First/last roughly five-minute phone late deltas were1/49, while capture
drop deltas were24320/4480. Host late deltas were51/0. Different workloads and
clock observations prevent causal comparison with the earlier50k run.

Four additional deterministic scenarios use existing seams; no runtime, queue,
deadline, security or codec behavior changed:

- **Protected rollover:** SRTP-authenticated SEQ65534/65535/0 and
  RTP timestamp0xfffff880/0/1920 reach `ReceiveState::receive` with increasing
  extended counters, three accepted packets, no late/future rejection.
- **Known active/zero/DTX accounting:** continuous Opus/SRTP processes20 active
  and100 intentional-zero40ms slots. A deliberately expired active packet is
  dropped before encryption;119 transmitted packets decode, one PLC slot retires
  once, and legal tiny DTX packets remain valid. The known active denominator is
 800ms; the intentional40ms source loss is5%, not0% just because all sent packets
  decoded. This synthetic material is not a listening/intelligibility test.
- **Independent relative clocks:** a45-minute virtual40ms source and nominal sink
  retain the current fixed80ms receive mapping and200ms future bound. Sender
  admission correction alone does **not** compensate remote receive-clock drift:

  | Source relative to sink | First late/future rejection | First PLC | Rejected frames / PLC slots |
  | --- | --- | --- | --- |
  | −500ppm | Late160.040020010s | 160.040s | 63501 |
  | −100ppm | Late800.040004000s | 800.040s | 47501 |
  | +100ppm | Future1600s | 1600.240s | 27496 |
  | +500ppm | Future320s | 320.240s | 59496 |

  Encoded occupancy stays at most six packets and native PCM one640-sample frame;
  bounded memory is **not** successful skew acceptance. Virtual arrivals use
  `admit`/`tick`; authentication is covered by the separate protected scenario.
- **Already-admitted competitor debt:** existing protocol builders/parsers yield
 8234 framed bytes for an8192-byte BLOB_PUT and16358 for a maximum permitted opaque
  SEND (Noise16/prefix2 included once). Under a conservative FIFO debt model,
  serialization takes1.31744/2.61728s at50k and0.8234/1.6358s at80k; three maximum
  SENDs take7.85184/4.9074s. All can exceed the frozen720ms progress limit, so the
  negative case must retire rather than silently enlarge credit. The other role's
  meter is untouched. This test debits the existing meter, **not** actual competing
  C/QUIC streams, production bulk pause, TEXT durability or a new scheduler.

The68-test checkpoint preserved these deliberately negative cases. The receiver
correction below now adds a separate before/after model; it does not rewrite the
old uncompensated result or claim that the historical80k run used the correction.

### Authenticated receive clock and actual sink compensation

The source SR now pairs monotonic NTP with the **currently projected RTP sample
clock**, not the most recently committed packet's timestamp. The immutable source
reference uses absolute sample time; the application capture-age floor is not an
acoustic clock. Physical frequency derives only from validated hardware frame/time
spans. Physical SR timing waits for a20-second span; until then the same protected
RR/SDES/APP still carries terminal feedback. Synthetic nominal input can report
immediately; missing physical timing never silently becomes nominal.

That source readiness avoids a reproduced startup failure class: ±0.8ms hardware
quantization over short spans produced15/16 rejected projected-clock reports. The
long-span version emits its first SR at20s and establishes a valid remote clock
at40s with zero rejections for each modeled±100/500ppm case. This is not causal
attribution of all previous Moto rejections or proof of HAL timestamp accuracy.

Only after whole-compound SRTCP authentication and strict profile validation does
the receiver expose typed NTP/RTP deltas. Its20-second rate window uses source
timing, **never packet-arrival slopes**. Reversed, discontinuous, stale or unsupported
reports freeze the established ratio and require a fresh healthy window. Health
expires after2s and is false for retired owners; unknown/default timing remains
48000ticks/1e9ns with calibrated/valid false. Supported±500ppm has a2ppm quantization
allowance; subsequent supported corrections are bounded to100ppm per window.
RTCP sizes remain116/96 plaintext and152/132 framed bytes; key/nonce/replay policy
and terminal admission semantics are unchanged.

The first authenticated RTP arrival+80ms phase remains immutable. Source-rate
projection now drives decode/skip/end deadlines and future validation; sink lead
uses actual consumption rate. Cursors never rewind. The protected-report model
covers45min ×20/40/60ms ×±100/500ppm:990000 source frames decode once with zero
late/PLC/future rejection/skipped slots/render loss. Encoded, native and sink queues
retain their original caps. Delay/burst/HOL100/300/1000ms, stale timing, transitions,
rollover, authentication tamper and retirement regressions remain passing.

Android separately calibrates actual AudioTrack frame/time progression over a
steady20-second window, rejecting underrun/rate-discontinuity samples. The platform
actuator chooses15992–16008Hz; fractional-Hz targets are averaged through one current
command per second, rather than accumulating the62.5ppm integer-Hz residual.
Unsupported relative clocks and missing/stale hardware references remain explicit.
Manual fixture override and restore remain tested. No custom resampler, route
authority, production call API or new packet format was added. The Linux fixture
sink follows the authenticated ratio with exact fractional accounting but remains
**virtual**, not hardware/acoustic evidence.

Current checks:79 focused probe tests,2 CLI tests,334 workspace tests and126 JVM
tests pass; the same two explicit native ignores remain. Formatting, arm64/API26,
debug/release and instrumentation builds pass. The latest normal DNS gate≥60s
also requires calibrated/healthy authenticated source timing and actual AudioTrack
compensation rather than merely logging those fields.

Available Moto+host actual DNS records use the private direct-authoritative LAN,
existing BBR selection and **synthetic50k framed-ingress service**. Declared F0=600ms
and capacity are not measured recursive-DNS safe capacity. Pre-teardown counters:

| Run | Moto decoded / late / PLC | Moto capture / render expiry | Last healthy host late / PLC | Clock observations |
| --- | --- | --- | --- | --- |
| Initial correction120s | 3004 /0 /0 | 4480 /0 | 30 /32 | Both valid by end;14 host clock rejects retained |
| Before source readiness60s | 1504 /0 /0 | 31360 /0 | 27 /71 | Host still uncalibrated after14 rejects; negative result retained |
| Latest source-ready60s | 1504 /0 /0 | 3840 /0 | 41 /42 | Both calibrated/valid, zero clock rejects; actual hardware compensator healthy |

Latest capture3840 includes3200 calibration plus640 additional loss; neither is
hidden. Moto has one AudioTrack underrun and710B max unconfirmed media; terminal
cycle423ms. Latest model read/authenticated140162/140162 and205557/205557 bytes
at61336ms, no live/retired partial tails. Probe cleanup4ms/native join<1ms rounded;
retired clock health is false. Intentional teardown PCM discard and expected host
progress expiry are separate from the healthy interval. These packet/PCM counters
are **not** source-active speech percentages, mouth-to-ear or listening scores.

The implementation and available source/model/foreground-device checks at61d0b80 were
complete. Full latest-build two-Android50/80 long, common-clock acoustic, actual
source-active loss/listening/double-talk and representative recursive-path gates
are not qualified. Pacman remains withdrawn by the user; a synthetic host is not
an authorized second Android. The goal records that physical-gate dependency as
blocked, not a successful conversational-voice acceptance. Moto-owned fixtures
and microphone permission were cleaned after results; Main and identities remain.

### Moto + Mi A2 Lite continuation: wall clocks and continuous source phase

The user supplied a new API30/arm64 Mi A2 Lite and explicitly requires operation
with intentionally incorrect device time and different timezones. Device clocks
and timezones are **not changed**. Deadlines remain monotonic; rate estimation uses
authenticated deltas from the same source, never cross-device absolute UTC. Signed
standard NTP conversion also permits pre-Unix dates, fractional negatives and NTP
era wrap. Exact zero timing still sends protected RR/APP, not fabricated SR timing.
Protected regressions cover different wall epochs and later forward/backward jumps.

Two physical60s attempts exposed persistent ShortRate64 failures (54/8 reports),
not an absolute-time or NTP-zero error. Ongoing valid±0.8ms hardware quantization
reproduced a publisher defect: applying each newest frequency to the entire prior
timeline rewrote already-published phase. A continuous fractional RTP accumulator
now integrates the old frequency to the change boundary, then changes only the
future slope. Two fixed hardware-point mean windows reject duplicates and cancel
first-anchor error; callback/network timing does not establish frequency. The
45-minute±100/500ppm traces authenticate54004 compounds with zero rejected reports,
frequency error≤2ppm and source-phase error≤48 ticks(1ms). Strict2500ppm short and
502ppm long guards, original media phase/budgets/queues and crypto remain unchanged.

Android discards an invalid initial20s hardware window and collects a new one,
instead of poisoning calibration with a transient advancing-but-stalled startup.
An established neutral ratio stays frozen. Timestamp observations are timed after
the API readback. Whole-measurement progress compares actual hardware timestamps
across platform-rate changes, not whether the latest-rate telemetry segment has
already accumulated two observations. Static first-failure rate/arrival masks
are observations, not alternative validation or a wire change.

Checks:87 focused,342 workspace,127 JVM; two unchanged explicit native ignores.
Formatting, arm64/API26 and complete debug/release/instrumentation builds pass.
Mi's packaged codec and local8s microphone/actuator/cleanup gates pass, as do Moto's
local gates. Actual two-Android120s direct-authoritative LAN/BBR with synthetic50k
framed-ingress service passes clock/lifecycle checks on both. Pre-teardown:

| Endpoint | Encoded / decoded / late / PLC | Capture ready→running | Render expiry | Remote clock rejects | Cleanup |
| --- | --- | --- | --- | --- | --- |
| Moto | 2996 /3000 /3 /3 | 33280→40960 | 0 | 0 | complete, native join35ms |
| Mi | 3002 /2576 /415 /427 | 3200→3200 | 0 | 2 ArrivalInterval16 | complete, native join1ms |

All415 Mi late packets are admitted after nominal deadline, not early sink PLC.
Their admission instant follows lane delivery/SRTP parsing, so this is not physical
DNS-arrival telemetry or proof of one transit cause. Clock calibration succeeds;
the asymmetric delivery is explicitly **not quality PASS**. A comparable80k service
attempt fails Mi's unchanged nonzero-microphone assertion (all-zero PCM), not crypto
or a claimed bitrate result. Capacity/F0 remain declared inputs, the byte-service
meter is synthetic, and acoustic/source-active/listening acceptance remains open.

### Exclusive codec owner and paired measurement retirement

The current-thread endpoint used to execute Opus inline with lane reads and
control/media admission. Independent held-encode/held-decode regressions reproduce
that scheduling failure. One private `m1v-codec` owner now exclusively owns encoder
and decoder, with **one** operation and one result. The actor selects completion
alongside authenticated RX/control/timer/cancel. The existing assembled source and
native render frame are the work slots, not an added job backlog. Cancellation
erases unobserved audio and joins the owner before generation replacement.

Ready decode is reconsidered when the actor wakes/worker becomes free; a ready
completion is consumed once before another select. A paced production-path model
reproduced timer-only dispatch delay at99.5ms and fixes it without changing source
age, byte credit, jitter, source cursor or presentation end. Publication time still
governs PCM expiry: finishing C decoding earlier cannot authorize late output.
The model did not reproduce the first physical160-sample tail expiry; that observed
local failure and its separate teardown totals are retained, not reclassified.

Lane envelopes carry local completed-body/Noise timestamps. Fixed8-bin durations
use inclusive upper bounds1/2/5/10/20/40/80ms and above for encode, decode, PLC,
post-Noise dequeue, validation and source-ready→Noise commitment. These are **local
Rust** diagnostics, not socket/DNS/acoustic arrival. The post-SRTP first-arrival
anchor and acceptance deadline remain unchanged. Crypto failures do not contribute
trusted admission metrics.

For a physical pair only, an optional `probe_finish_marker_path` must be the
Android-canonical private `filesDir/voice-probe/<run>/finish.marker`, absent at setup
and interval end. Each test freezes running state/duration, emits structured
`call_probe_interval_complete`, and continues producers for at most15s while the
controller atomically renames a prepared UID-owned0400 one-byte0x01 marker after
**both** interval reports. Canonical path, regular file, UID, mode, length,
no-follow open/inode and payload checks are enforced. Barrier time is excluded
from the frozen interval. No marker argument preserves single/local behavior;
clock, microphone, timing, progress and cleanup assertions are not weakened.

Current checks:99 focused,2 CLI,354 workspace and127 JVM tests pass; the same two
explicit native ignores remain. Arm64/API26, debug/release and instrumentation
builds pass. Both subsequent strict local8s gates pass: Moto206 decoded and Mi209,
late/PLC/render expiry0, while source capture losses remain observable.

The latest120s pair uses actual Moto/API35 and Mi/API30 C/DNS/audio paths on the
direct-authoritative LAN, existing BBR, no synthetic service meter, declared
admission50k/F0=600ms, and finite known PC-speaker test speech. Both frozen
clock/lifecycle/cleanup gates pass; both clocks have zero rejected reports.

| Endpoint | Decoded / late / PLC | Capture ready→running | Render expiry | Post-Noise deadline crossings |
| --- | --- | --- | --- | --- |
| Moto | 2990 /10 /10 | 27520→101120 | 0 | 0 |
| Mi | 2888 /4 /119 | 3200→3200 | 0 | 0 |

Moto's73600 post-ready lost samples are115 frame-equivalents, consistent with
Mi's119 PLC minus4 late; this is not packet-correlated or active-speech accounting.
The earlier unmetered timing pair had Mi320 late,318 already late at Noise
completion and2 crossing during local post-Noise work. Repeated workloads and
initial phases differ; the improvement does not attribute every prior loss to
codec blocking. The newly observable remaining capture loss is not erased.
Capacity and F0 are declared, not measured recursive-DNS service; no QPS, latest
long-run, common-clock acoustic or human listening acceptance follows from this
plumbing result. Main, identity, native pin/scheduler and device wall clocks remain
unchanged. Gate fixtures and microphone grants remain only for authorized tests.

### Source collection, bounded render preparation and sink qualification

A held decode operation owns the render slot, not the assembled source frame.
The actor now collects into that original free source frame while decode runs;
encode/result ownership, the four160-sample input ring, full-frame/pending/commit
guards and the40ms additional-age limit are unchanged. Deterministic15/25/35ms
held-decode cases previously lost640 valid capture samples; the corrected slot
ownership collects them without increasing memory or moving source deadlines.

Present authenticated packets may start computation with a20ms compute reserve
inside the original80ms timeline. Missing packets retain the10ms preparation
reserve. Native pulls gate each next untransferred sample against its original
source time and the actual rate-aware sink lead; earlier computation cannot play
early, move the immutable frame end or rescue an expired completion.

`last_render_expiry` is one optional bounded diagnostic, not a history queue. It
records queued versus completion expiry, publication/codec/pull offsets, transferred
and discarded samples, original frame duration and last declared sink state. All
times are local monotonic offsets; returned native PCM is not proof of AudioTrack
acceptance or acoustic presentation. The latest frozen running snapshot retains
active evidence even if later teardown replaces the record. Earlier intermittent
Moto320-sample tail expiry is not declared causally resolved: subsequent8s/16s
local checks have no expiry, but the old failure remains recorded.

The optional local duration argument `probe_local_duration_seconds` accepts8–16s
(default8), retaining strict active zero-render-drop and platform actuator checks.
One new exact source diagnostic, `foregroundMicrophoneSourceDiagnosticOnlyInGatePackage`,
compares6s MIC then6s VOICE_COMMUNICATION with the existing effects in a visible
gate-only host. It emits only counters, route/effect/timestamp and cleanup evidence;
both-zero input fails. One actual Mi result has89802 nonzero MIC samples but zero
processed samples, despite enabled effects and hardware progress. The sequential
source/effect comparison is not isolated AEC/NS causality or voice acceptance, and
the original live gate still requires a real nonzero microphone signal.

Initial AudioTrack calibration now qualifies two adjacent≥10s hardware spans over
at least20s. Their slopes must agree within one-frame endpoint uncertainty. A
reproduced18.34ms startup pause previously froze approximately−919ppm and made a
steady16kHz sink unusable; the contaminated window is now rejected and a subsequent
steady window calibrates without rebasing an established ratio. Supported frequency,
freshness, actuator, jitter, queue and physical assertions are unchanged.

Checks:110 focused,133 JVM and complete native/debug/release/instrumentation builds
pass. Workspace default-native checks pass365 tests plus the separately implemented
TCP seam test; two existing explicit native ignores remain unchanged. A120s actual
Moto/Mi pair with synthetic50k framed-ingress service and qualified sink clocks
passes both clock/lifecycle/cleanup gates, with the following frozen counters:

| Endpoint | Decoded / late / PLC | Capture ready→running | Render expiry | Already late at Noise / post-Noise crossing |
| --- | --- | --- | --- | --- |
| Moto | 2997 /4 /4 | 27520→28800 | 0 | 2 /0 |
| Mi | 2757 /241 /243 | 3200→3200 | 0 | 239 /0 |

Moto additional loss is1280 samples; Mi delivery remains variable and most lateness
precedes completed Rust Noise authentication. The earlier comparable trace had
36 Mi late packets, so neither clock/lifecycle PASS nor this comparison qualifies
the profile. Service/capacity/F0 remain synthetic or declared; no common-clock
acoustic, source-active loss, listening, measured recursive capacity or latest
long-pair result is claimed. Device clocks/timezones, Main, pin and scheduler remain
untouched. The next TCP-only candidate is opt-in and remains a causal experiment.

### Opt-in TCP seam, ordered clock bursts and partial renderer writes

The default build still leaves the pinned C TCP socket options unchanged. The
private `voice-probe-tcp-nodelay` core feature (and corresponding
`android/build-native.sh --voice-probe-tcp-nodelay`) enables a staged TCP-only
patch for the accepted client FD and outbound server target FD. Both creation
seams check `setsockopt(TCP_NODELAY,1)` and exact `getsockopt` readback before
stream ownership; failure follows the existing close/reset path. AF_UNIX tests,
FIN semantics, receive windows, DNS/QUIC/CC/scheduler and the transport Git pin
are unchanged. The disposable CLI server must also be built with Meson's
`-Dprobe_tcp_nodelay=true`; the production Dockerfile is not changed.

The isolated `crates/slipstream-sys/tests/new_probe_tcp_gates.py` runner completed
one clean invocation: default and enabled Cargo/seam checks, all four pinned
Meson suites in both modes, and enabled embedded loopback with PEM/DER pins,
wrong-pin rejection, eight exact streams and terminal-loss cleanup. Earlier
intermittent fixture failures remain recorded; assertions were not suppressed.
Baseline binaries are hash-checked and not overwritten. This is evidence of the
socket option and ownership, not a causal Nagle or voice-quality result.

A separate protected clock regression reproduced an invalid arrival-frequency
restriction. Authenticated reports spaced100/200ms in source NTP can legitimately
arrive10ms apart after bounded ordered-stream delay. Arrival now must progress
strictly and stay within2s, while the minimum50ms interval remains on source NTP.
Frequency still uses only authenticated NTP/RTP deltas. Existing source/packet
progress,500ms arrival/source divergence, replay,2500/502ppm rate checks,
20s calibration and2s freshness are preserved. Duplicate/backward arrival still
fails; valid debunching no longer invalidates a calibrated clock.

The renderer's fresh pull still reserves160 samples, but a pending partial write
reserves only its actual remainder. Depth560 plus pending80 can finish at640,
rather than unnecessarily parking until depth480. Five JVM regressions retain
partial/zero-write ownership and the original640-sample sink cap. The correction
does not prove the cause of a missing subsequent native pull or resolve the
historical Moto tail expiry; the bounded Rust sink model uses the same predicate.

Current checks:112 focused,368 workspace (two existing explicit native ignores),
138 JVM, formatting, native arm64/API26 and full Android builds pass. The candidate
client/server build and both phone APKs are labelled separately from the default.
All following120s records use two real Android/C/DNS/audio endpoints, existing
BBR, the opt-in TCP option, synthetic50k framed ingress per role, declared
admission50k/F0=600ms, a finite PC-speaker test stimulus and coordinated interval
retirement. Both clock/lifecycle/cleanup checks pass, with zero clock rejections.

| Packet duration | Moto decoded / late / PLC / expired samples | Mi decoded / late / PLC / expired samples | Post-ready capture loss Moto / Mi |
| --- | --- | --- | --- |
| 40ms | 2974 /27 /27 /160 | 2918 /82 /85 /0 | 1920 /0 |
| 20ms | 5958 /44 /44 /1440 | 5948 /63 /64 /640 | 320 /0 |
| 60ms | 1984 /17 /17 /0 | 1984 /18 /19 /0 | 960 /0 |

The40ms Mi trace has78 packets already late at Rust Noise completion and one
post-Noise crossing. The20ms Mi last expiry is a whole320-sample completion;
Moto still shows queued tails. The60ms sample has no expiry but longer
packetization and more20–40ms codec operations. Workloads and first-arrival phases
differ, so these short records do not select an optimum, demonstrate p95
mouth-to-ear, or establish active-speech loss. Earlier reverse baseline has
Mi418 late and candidate12 late, but candidate clock health failed; that is not a
causal promotion of TCP_NODELAY. Admission capacity and healthy feedback cycle
remain declared, the service ceiling is synthetic after DNS transit, and the
direct-authoritative fixture supplies no public-recursive capacity or DNS-QPS
acceptance. Latest-build long-pair and common-clock acoustic evidence remain open.
