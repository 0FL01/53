# Live call client API

Production calls reuse `live_media`, extracted from baseline `35a6b56`.
`voice_probe` retains diagnostic fixtures; production never calls its
`start_local`/`start_dns` or starts a second carrier.

## Control and ownership

`DmsgClient.start_calls(observer)` returns `DmsgCalls`, one persistent actor per
canonical core DB path. Kotlin bindings are generated, not handwritten.

- `dial(contact_id)` reserves one outgoing attempt; `accept(call_id)`,
  `reject(call_id)`, `end(call_id)` act only on the matching current call.
- `end("")` cancels the current outgoing preparation generation, never an
  unrelated incoming call. UI also clears its not-yet-submitted dial.
- Commands run outside the Android store lock. End/reject/shutdown use the
  existing generation-scoped urgent path, independent of the four-command queue.
- `shutdown()` joins the actor/media before releasing its DB-owner lease.
- Observer snapshots: call_id, contact_id, phase, direction, reason,
  media_handle, available. Initial `Listening/available=false` is startup,
  not terminal `Stopped`. Wait for availability before submitting a dial.
- Phases: Stopped, Listening, Dialing, Ringing, Connecting, Active, Ended.
  `Connecting` supplies the native audio handle after all media attachments;
  `Active` reflects media readiness, not a measured conversational-quality PASS.

The actor owns three independently RESUME-authenticated Noise streams over the
existing `dns::endpoint`: signaling, RTP and SRTCP. Attachments use consuming
`DirectTcp::into_lane_parts`; Noise is never cloned. Idle signaling may reconnect;
an active call never resumes/replays after transport loss. Each redial has fresh
call ID, directional SRTP keys/salts, SSRCs and sequence/timestamp origins.

Ephemeral Olm uses the existing accepted-contact/identity/prekey policy and
IMMEDIATE ratchet transactions. Successful call crypto writes account/sessions,
not messages/history/outbox/blob. Invalid envelopes do not consume persisted
ratchet/OTK state. A post-commit local contact block cancels a matching call,
including while ordinary contact synchronization is paused.

## PCM and teardown

`org.dmsg.client.CallMediaJni` consumes a core-issued handle; it has no public
fixture/codec startup. Bounded PCM stays on JNI, not UniFFI:
push≤160 samples with AudioRecord frame/timestamp; pull≤160; sinkQueued≤640;
sourceRouteChanged, sinkRate, renderExpiredSamples, stats and stop.

The call actor owns `MediaSession`; the Android driver owns AudioRecord/Track
and capture/render threads. Android stops/joins both PCM workers before JNI
retirement. Native stop closes/joins endpoint, codec and lanes, zeroizes PCM and
notifies handle retirement. Calls ending without audio still retire Telecom.
Failed/stopped native media resolves through the actor's existing terminal path;
Android does not replace that reason with AudioUnavailable merely because JNI
retirement finished first. Physical capture/playback/route failures still end
with AudioUnavailable, including real hardware frame/time reversal.

`DmsgService` owns `CallSessionOwner`, one Core-Telecom call and the audio driver.
Activity pause/recreation only detaches UI observation. Telecom owns focus/mode
and route requests; the debug probe retains standalone platform ownership.
After disconnect, cancel the three call-scope flow collector children and join
the scope so Core-Telecom1.0.1's normal close path executes. Do not merely join
open-channel collectors or race SDK close with whole-job cancellation.

Note audio is interrupted/joined before capture and cannot claim ownership until
call teardown finishes. Message/bulk owners pause without `dnsStop`; TEXT with
an existing Olm session remains queued. End wakes normal message/bulk processing.
Microphone permission is requested only from visible UI; no capture during ring.
Mute zeros outgoing PCM without changing positions/global mute/volume. A permitted
Telecom route change resets local hardware calibration, not RTP/key epochs.
Its explicit monotonic publication marker is serialized with capture pushes.
Qualified fresh hardware timestamps may account complete missing 160-sample
batches across the sampling pause in RTP source position; raw AudioRecord read
positions and capture-age/reversal checks stay unchanged. The conservative
gap excludes existing 2ms/±500ppm uncertainty and is included in capture loss
(`source_route_gap_samples <= dropped_capture`). Missing samples never enter
measured frequency: a new actual 20s span must qualify, while the source/NTP
epoch and published SR frontier remain intact. Unmarked/invalid gaps are not
rebased, and receiver deadlines/queues are unchanged.

Last numeric SRTCP indices identify preparation, Noise commitment, server
ingress/whole TCP writes/Full drops and authenticated client receipt. They are
diagnostics, not credit or proof of timely peer delivery. The server yields
after successful media ingress so a ready peer can drain its existing one-slot
queue; capacity, 20ms hop deadline and Noise ordering remain unchanged.

## Fixed media and acceptance

Mono16k/12kbit/40ms/cap100, encoder10/decoder7, DTX/PLC, jitter80ms, sink640,
F0=600ms/ledger720ms/window2880B and75% admission remain unchanged. Production
declares50k useful capacity; it introduces no synthetic service and does not claim
measured DNS C_safe. No automatic BBR/TCP_NODELAY experiment promotion.

Current physical results and remaining gates live in
`docs/goals/2026-10-11-call-client-apk.md`. Host tests, control readiness and a
signed APK are not proof of sustained phone-to-phone conversational media.
