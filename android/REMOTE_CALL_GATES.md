# Moto ↔ remote OnePlus call gates

Execution contract/status: `../docs/goals/2026-10-11-call-client-apk.md`.
This route replaces the failing Mi audio endpoint; it does not relax call acceptance.

## Targets and boundaries

- Local Moto: USB `ZY22JFJ5LP`, device `cancunf`, API35.
- Remote OnePlus: USB `2bcd17fa` on the authorised remote ADB host,
  device `OnePlus7Pro`, model `GM1911`, API36/arm64.
- The user reports about700km physical separation and possible internet allowlisting.
  These are reported conditions, not measured latency or a diagnosed blockade.
- ADB/SSH only install/provision/orchestrate selected `.gate` methods. No TCP port
  forwarding, host proxy, DirectTCP media or authority substitution counts as the
  remote recursive-DNS call. Application transport remains the pinned native client.
- Preserve Main, existing accounts/Keystore, user volume and crypto/pin validation.
  Fresh gate onboarding is explicit; do not add automatic APK wipes.
- Secret credentials/invitations stay in owner-only files and SSH-protected file
  transfer/stdin, never argv, environment, Git, raw logs or APK. No audio files/STT.

## Selected verification sequence

1. Verify both hardware serials/device identities before each installation/test.
2. Install matching gate target/test APKs with `adb -s SERIAL install -r`.
   Never use `connectedDebugAndroidTest` or install/reset Main.
3. On OnePlus, run exactly
   `CallMediaProbeGatesTest#foregroundMicrophoneProtectedLoopbackAndCleanupOnlyInGatePackage`
   with `probe_local_duration_seconds=16`, `probe_loudspeaker=true`,
   `probe_local_exercise_rate=false`. Preserve all source/sink/cleanup assertions.
   This is a local audio test, not network acceptance. Restore probe-OFF native.
   Record startup route failures separately. An earpiece contrast and the existing
   microphone-source diagnostic may isolate them; neither waives the original
   protected fixture's nonzero-input or render-loss assertions.
4. Use current `VoiceDnsGatesTest#bootstrapFreshDnsVoiceAccount` and
   `#acceptPeerOnExistingDnsAccount` for reciprocal contact acceptance after signup.
   Retain the existing Moto account; select only new named owner-only fixtures.
   Export public QR with an exact bounded `exec-out run-as cat` read: `dd` statistics
   must never be appended to QR bytes. Restore public fixture mode0400 after updates.
5. Coordinate the existing genuine `CallClientGatesTest` receiver/caller methods
   across independent ADB hosts. Wait for the receiver's formal listener-ready
   receipt before dial. Run reject/no-microphone, short media, then180s continuous
   capture/decode/sink/hardware-clock checks after a passing short call. Include
   UI/Telecom route, recreation/background/lock, normal End and joined cleanup.
6. Verify post-call encrypted TEXT and the existing applicable VOICE path. Preserve
   durable data and report failed network/lifecycle methods as failures, not skips.

## Evidence interpretation

Require actual `OK (1 test)`, nonnegative status codes ending in0, final code−1
and ADB return0; code−1 or process success alone is insufficient. Emit only formal
allowlisted numeric/boolean diagnostics. Independent phone timestamps/final
snapshots are not a common clock or timely covering-ACK proof.

Record selected DNS input separately from the transport's actual primary/fallback
tier: the ordinary supervisor can use its Yandex backup. A saved input assertion
does not prove the active destination. Host DNS/validated Wi-Fi does not prove
Android messenger reachability or absence/presence of internet allowlisting.

## Current receipt

- Identity matched `2bcd17fa` / `OnePlus7Pro` / `GM1911`; Android16/API36/arm64.
- Android default network: Wi-Fi, INTERNET/VALIDATED, no captive portal,
  one platform DNS server. Linux-host bounded public DNS query also succeeded;
  this is host-only capability, not application-path acceptance.
- No messenger package at preflight. Matching `.gate`/`.gate.test` installed-r;
  microphone and notification permissions externally granted, no reset/Main install.
- First speaker fixture failed during startup with `input_route_changed`; cleanup
  completed. This is not the Mi 105ms write/loss mechanism.
- Earpiece contrast ran16.044s:402encoded/409decoded, active render expiry/drops,
  late/PLC and underruns all0; buffer640/capacity1288/performanceNONE, no slow-write
  witness, cleanup complete. The actual framework still FAILED: processed microphone
  peak0 violates the unchanged nonzero-input assertion. Do not label this local PASS.
- Separate6s MIC/6s VOICE_COMMUNICATION diagnostic passed: peaks47/173,
  nonzero samples87002/7399, clipping0, both cleanup receipts complete. This proves
  available input counters, not intelligibility, continuous speech or call acceptance.
- Production/probe-OFF targets installed and hashed on both devices. Fresh OnePlus
  pinned Android DNS signup and reciprocal accepted contacts passed; Moto account
  and Keystore preserved. Original exec-raw APK transfer timed out; SFTP succeeded.
- Remote incoming Reject/no-microphone method passed over genuine DNS. Moto reached
  REJECTED, with no capture, but the original joined-retirement guard failed:
  tearing_down/telecom_ready remained true. Fixed nonsecret teardown checkpoints
  now distinguish platform disconnect outcome wait from the subsequent SDK join;
  no extra timeout, whole-job cancellation or relaxed acceptance is introduced.
- The diagnostic repeat passed both actual Reject frameworks with joined Telecom
  and no microphone. The first retirement failure did not reproduce; diagnostics
  are not claimed as a causal fix.
- First10s media run: genuine bidirectional RTP, Moto mute/unmute and same-call
  recreation occurred, but both actual frameworks failed. OnePlus ended with
  ConnectionLost/nativefailed=false; the last Moto report was still ACTIVE.
  One allowlisted pre-retirement snapshot will classify the caller failure before
  any long-run acceptance. No180s or conversation PASS is inferred.
- The pre-retirement diagnostic classified the next10s failure: Moto oldest RTP
  age720447us exceeded720000us;130encoded/126decoded/129received, render expiry0,
  no hardware reversal. OnePlus ended secondarily with136encoded/92decoded/127RTP
  and nativefailed=false. Server SRTCP ingress/writes29 each, Full/expiry0; whole
  TCP writes/final markers are not timely peer-delivery proof. A separate finite
  Moto LTE→Wi-Fi contrast restores the prior settings and changes no media bounds.
- That separate contrast failed both frameworks with typed Wi-Fi on both phones:
  OnePlus oldest63799 age721506us/frontier63798,40encoded/33decoded/33RTP;
  Moto46encoded/13decoded/36RTP ended secondarily. OnePlus render expiry1920,
  underruns0, no hardware-reversal witness. Moto Wi-Fi0/mobile-data1 restored.
   The existing isolated open-STREAM early-repeat comparison also failed on this
   replacement pair: OnePlus28encoded/14decoded/19RTP, control-lane failure,
   max feedback718ms, no hardware-reversal witness. The caller receipt was lost
   by a collector API error; its cause is not inferred. Both client and carrier
   were restored to9f, account/config/data unchanged. No180s run followed.
- The known public-authority control failed too: Moto oldest54819 age720632us,
  OnePlus oldest28081 age720734us, both actual frameworks failed. Identical guarded
  input in both native attempts removed fallback ambiguity; this control was not
  recursive acceptance. Platform inputs restored; ordinary fallback restored in source.
- Source-confirmed recovery correction7d3c9dd excludes ACK/POLL-only packets from
  the PTO anchor, preserving existing RTO/backoff/RACK. Real C before/after gates pass;
  matching clients/carrier were installed, but both actual10s frameworks failed:
  Moto oldest57470 age726224us and OnePlus oldest18320 age721022us. No180s followed.
- The bounded numeric native snapshot is implemented and physically forwarded:
  the existing C worker publishes at most8 path slots and scans at most64 pending
  nodes; foreign readers copy only atomic control storage. The existing DNS
  supervisor and call stats/stop JSON forward an optional allowlisted object.
  Native ages describe capture time; observation age is only a lower bound and
  publication freshness stays unknown. No addresses/CIDs/packet bytes or clock
  mapping are exposed. Host449 tests/2 unchanged opt-in ignores, C concurrency/layout
  and closest JVM checks pass; complete Android JSON/16KiB forwarding passed too.
  The actual10s call still failed both frameworks: Moto oldest33002 age725048us.
  Final fresh ACKs/PTO0 and young pending packets do not map to that app record.
- One existing TCP_NODELAY seam control compared both7d clients/carrier on
  this replacement pair, preserving default fallback,600/720/2880/80/640 and pins.
  Previous TCP controls on older sources/Mi remain negative; this is not a claimed
  fix or180s acceptance. Both actual frameworks failed before media: MotoUnavailable,
  OnePlus listener ready/DNSReady without Ring; native snapshots were null. The
  unavailability cause is unknown, and no feedback-delay comparison was obtained.
  Exact carrierc702/config and default/probe-OFF/TCP_NODELAY-OFF target SHA0b848d0c
  were restored and hash-verified on both phones, unchanged test SHA47818d1a;
  backendb580 ID/image/data and account/Keystore identities are unchanged. Both
  gate processes stopped. Next discriminate exact control-stream byte boundaries,
  not an unchanged long retry or retention of the experimental seam.
