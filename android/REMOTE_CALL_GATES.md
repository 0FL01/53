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
- The selected SRTCP byte-frontier diagnostic is now implemented and host-verified:
  absolute Rust socket counters include IK/domain/login/attach before lane handoff;
  partial IO, completed records and Noise indices are separate markers. The native
  owner publishes only the selected stream's submission/contiguous/flush/pending
  offsets through a separate optional ABI. Private loopback port/generation never
  appear in JSON; retirement clears the selection, with no stale-lane fallback.
  Host456/2 unchanged native ignores,185 JVM tests, default ARM64/full Android and
  real317/911-byte selected-stream loopback pass with all8 original parallel streams.
  Target SHA112c26a9/reporter test SHA65808e1a is installed/hash-verified on both
  preserved identities; the full optional object passes the16KiB recorder guard.
  Native submission is not an ACK; C flush is not Rust authentication. Publication
  age stays unknown, and `rx_noise_srtcp_index` is not SRTCP/ledger authentication.
  Freeze the last pre-expiry observation, not later teardown progress. The genuine
  10s call below preserved identities/default carrier; maximum recorder line6353B.
- The schema2 selected-stream diagnostic adds `rx_highest` from the accepted
   STREAM end offset (`fin_offset`), including zero-length ranges/FIN. A difference
   from contiguous is an end-offset gap, not buffered suffix-byte count. Real
   pinned decode/bridge flush verifies12/0→12/4→12/12, duplicate suppression and
   zero-length16/12. The C worker remains the only reader; the separate ABI is14
   words, connection/path155 unchanged, with the same64 visits/3 read attempts.
   Actual coordinated10s fails both frameworks: Moto oldest14281 age720538us,
   OnePlus oldest58206 age720719us. Frozen receive highest/contiguous/flush/read/auth
   equals972/index6 on Moto and840/index5 on OnePlus. OnePlus fully wrote control7
   through1124, while its native capture submitted992; freshness is unknown.
   Independent server final received/wrote SRTCP7 each way, with control Full/expiry0.
   Target SHAa25dc7ed/test65808e1a is hash-verified on both preserved identities;
   complete forwarding passes the16KiB guard (maximum6339B), not call acceptance.
   Both gates joined/stopped; carrier/backend/data/default policy are unchanged.
    A subsequent private-generation barrier now binds native publication to a
    completed TCP record without adding clock mapping, queues, workers or policy.
    `publication_after_tx_record_end` must equal the paired socket's complete record
    end. A stable lease owns retirement; each newer completed write invalidates old
    native evidence until a matching worker publication arrives. Null remains an
    optional unavailable diagnostic, not an audio failure. Program ordering does
    not supply publication age, stall duration, stream ACK or timely feedback proof.
    Host458/2 unchanged native ignores,185 JVM tests/default ARM64/full Android and
    a same-socket73-byte post-write epoch refresh pass. Actual10s still fails both
    frameworks: Moto ledger720127us, OnePlus secondaryConnectionLost. During active
    Moto mute/unmute/recreation, a publication-after1256 reports submitted1124;
    this is a proven post-write difference, not its duration or failure cause.
    At expiry Moto write/submission1520 and all receive frontiers1368/index9;
    OnePlus later write/submission1652 and receive1500/index10. Independent server
    final SRTCP10 received/written each direction, Full/expiry0, is not timely credit
    proof. Target SHA3268e29b/test65808e1a is hash-verified on both preserved identities;
    remote recorder maximum5702B. Both gates joined/stopped; no180s run followed.
- Schema3 ACK-prefix transportc3e20dd is now matched on clients/carrier, with
  unchanged nested pins and defaultDcubic/probe-OFF/TCP_NODELAY-OFF/fallback.
  Workspace458/2 unchanged ignores, JVM185, Meson10/10, explicit loopback,
  ARM64/debug/test/release and31 protected normal/ASan/UBSan prefix cases pass.
  Target SHA9097fd77 (22135805B), unchanged test65808e1a, install-r/hash-verified
  on both preserved identities. Fresh formal readiness preceded immediate dial.
  Both actual10s frameworks FAIL: ADB0/final-1 but framework not OK/status-2.
  Moto oldest35151 age720317us/frontier35150,90encoded/75decoded/88RTP;
  outgoing record21/write/submission/ACK-prefix2972, incoming highest/contiguous/
  flush/read/auth2820/index20, pending0. OnePlus95encoded/68decoded/90RTP,
  secondaryConnectionLost/nativefailed=false; final write22/end3104, native=null,
  incoming2952/index21. Its terminal35167 was prepared/committed in22; server
  final accepted/wrote only21 SRTCP each way, Full/pre-Noise/route expiry0.
  These are independent endpoint/final markers, not shared-clock delivery timing.
  Recorder maximum5362B; optional forwarding passes, not R2. Matching carrier
  imagef5cb11ff/config metadata verified, backendb580 ID/image/data unchanged,
  rollbackc702 retained. Gate processes stopped, no reset or180s run. Next add
  finite receiver checkpoints inside the unchanged3s background interval because
  the final native=null cannot distinguish submission from ACKed prefix.
- Actual byte-frontier10s failed both frameworks: OnePlus primary oldest60970
  age720469us/frontier60969, incoming index27/socket read/auth3744 equals native
  contiguous/flush3744 with pending0. Independent server final wrote29 toward it,
  control Full/pre-Noise/route expiry0. Moto ended secondarily; its later record30
  was fully TCP-written4160 but native-submitted4139. Neither independent final
  marker proves timely covering feedback. Next highest received QUIC offset can
  distinguish later bytes behind a reassembly hole, without changing recovery.
  Formal fresh readiness is JSON `event=listener_ready, ready=true`, with no final
  marker; checking a nonexistent boolean field caused a prior undialed timeout.
  The first coordinated run lacked the instrumentation forwarding key; after its
   one-key reporter fix, the optional byte-frontier object is physically observable.

### Latest finite reporter checkpoint (2026-10-14)

- Product/carrier remain matchedc3e/default; target SHA9097fd77 is unchanged.
  Only test APK changed toSHA274a2787b44e0bdbd7e72bb09bd1322d7478ae98225254bfe820dde5ef612b39,
  962348B. JVM185/instrumentation build pass, install-r/hash verified on both phones.
- Finite receiver ACTIVE/1s/2s/3s observations preserve the absolute3s background
  interval. Short caller intervals report every1s without weakening continuing
  capture/decode/identity/cleanup assertions or adding a worker/queue/service.
- Fresh coordinated10s **PASS on both actual frameworks**: ADB0, OK1, nonnegative
  status codes ending0, final-1, normal local/remote End and complete joins.
  Moto final ACTIVE283encoded/272decoded/282RTP, render expiry0; OnePlus normal
  retirement292encoded/247decoded/287RTP, startup render expiry480, nativefailed=false.
- Receiver remains ACTIVE after3s background, with socket write/native submission/
  ACK-prefix3104 and all receive frontiers2952/index21. At1s native is null, not a
  reused observation. Remote maximum recorder line5745B stays within16KiB.
- Moto naturally reports2 native paths versus1 in the preceding failure; resolver
  profiles still match platform2/1 and Wi-Fi false/true. Active primary/fallback tier
  is unknown. The reporter change is not a demonstrated product/network fix.
- Server final SRTCP ingress/egress60/59 and59/60, Full/pre-Noise/route expiry0.
  Independent final markers do not supply timely covering-feedback proof.
  Both gate processes stopped; identities/config/data/pins/defaults preserved.
  This short PASS permits the next180s gate; it is not full R2 acceptance.

### Conditional180s result (2026-10-14)

- The permitted fresh180s attempt **FAILS both actual frameworks early**:
  framework not OK/status-2 despite ADB0/final-1. Moto52encoded/46decoded/51RTP,
  oldest7403 age720043us/frontier7402, last authenticated control185416us old;
  OnePlus58encoded/38decoded/46RTP, secondaryConnectionLost/nativefailed=false.
- Moto complete write/submission2048, ACK-prefix1916; incoming highest/contiguous/
  flush/read/auth1896/index13. OnePlus later complete write/submission2180,
  ACK-prefix2048, incoming2028/index14, committed index15/terminal7415.
  Its first1s background report is already ENDED, not a pre-expiry observation.
- Server accepted/wrote SRTCP14 each direction, Full/pre-Noise/route expiry0.
  A covering terminal for index14 is unknown; independent final markers are not
  timely covering-feedback proof. Default matchedc3e artifacts/config/data/pins
  and both identities are unchanged; gates joined/stopped after collection.
- No long acceptance or causal reporter fix. Next test the concrete stale native
  send-batch-time hypothesis locally, before selecting a policy-neutral correction.
  An independent locked-notification control uses secure Moto as receiver and
   OnePlus as genuine caller; do not provision/bypass OnePlus's absent keyguard.

### Current-pair locked diagnostic (2026-10-14)

- Six finite test-only notification checkpoints, JVM185/instrumentation build PASS.
  Test SHA df3aabaebf955ffeb5a25f2690e4166221a10bd734efee2cde83ecfced1cb606,
  962502B, installed-r/hash-verified on both; matchedc3e product/carrier unchanged.
- Moto observes CallStyle and Answer, sends the production notification PendingIntent
  and reaches ACTIVE while secure=true/locked=true. No PIN/keyguard dismissal,
  volume, identity or microphone-before-answer guard change. The first attempt
  failed before media without a precise assertion witness; added reporting is not
  a diagnosed product fix.
- The complete short reverse call **FAILS both actual frameworks**, status-2:
  OnePlus oldest3043 age721050us, terminal=null,19encoded/9decoded/15RTP,
  own write/submission1124/ACK-prefix1103, incoming840/index5. Moto later commits
  feedback8/terminal3057, own1256/submitted1256/ACK-prefix1124, incoming1104/index7,
  secondary control-lane failure. No entire lifecycle/screen-off interval PASS.
- Server SRTCP accepted/wrote7 each way, Full/pre-Noise/route expiry0; covering8
  timing is unknown. Both gates joined/stopped, identities/config/data/pins/defaults
  preserved. No180s retry. A disposable real-pacing reproduction independently
   supports a policy-neutral per-prepare native-clock comparison, not live causality.

### Matched per-prepare native-clock result (2026-10-14)

- Transport5ac8135614be47fedb1b7c25495e487829e20bc4 samples the same existing
  clock before packet preparation, retaining batch-time budgets/deadlines.
  Advancing-time pre/post regression,10 Meson suites, poll-submit ASan/UBSan,
  host458/2 unchanged ignores/fmt,185 JVM, default ARM64/full Android and explicit
  selected/post-write/original8-stream loopback pass.
- Target SHA0e0acaf91a8770457908cf36b71d4df2e664604652390df92fb107ec0c70e6fa,
  22136065B, and unchanged df3 test962502B are installed-r/hash-verified on both.
  Matching diagnostic carrier8655005b preserves normalized config/mounts/ports/
  network/IPAM and backendb580 exact ID/image/data/pins; rollbackc702 is retained.
- Fresh formal readiness/immediate dial10s **FAILS both actual frameworks**:
  ADB0/final-1 but framework not OK/status-2. OnePlus37encoded/30decoded/33RTP,
  oldest25588 age726005us/frontier25587; authenticated control188370us old.
  Its write/submitted1520/ACK-prefix1488, incoming highest/contiguous/flush/read/
  auth1500/index10, pending0. Moto later40encoded/23decoded/37RTP, commits
  feedback12/terminal25604, write/submitted1784/ACK-prefix1652 and incoming1500/
  index10, secondaryConnectionLost without a ledger witness.
- Server final SRTCP ingress/egress11/10 and10/11, RTP37/37 each way, all Full/
  pre-Noise/route expiry0. Covering-record timing remains unknown; final markers
  are not a common clock. OnePlus startup render expiry480/39.925ms accepted
  160-sample write is an association, not the ledger cause. Recorder maximum6413B.
- Typed Wi-Fi false/true and profiles match platform2/1; actual primary/fallback
  tier remains unknown. Both gates joined/stopped, identities and defaults remain.
  No180s. Next trace feedback/downlink scheduling before another source candidate.
