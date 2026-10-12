# Goal: клиентские аудиозвонки и подписанный APK

Status: blocked
Source: план «контакт → настоящий звонок → завершение → клиентский APK», утверждённый пользователем 2026-10-11; «реализовать и коммит пуш».
Last updated: 2026-10-12

## Objective
Выпустить подписанный release-оптимизированный `53.apk` с настоящими личными DNS-only аудиозвонками на обновлённом fresh msgd, переиспользовав media baseline `35a6b56` без нового цикла M1-V qualification.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: account-authenticated ephemeral signaling, Olm directional key exchange и opaque SRTP relay через существующий DNS endpoint.
  - Source: утверждённый план, steps 2–5.
  - Acceptance: accepted contacts выполняют Invite/Ringing/Accept/MediaReady/End; busy/TTL/early cancel/revoke/block не оставляют routes; отдельный Noise owner у signaling/RTP/SRTCP. Сервер не получает media keys/PCM; call state/audio не попадают в mailbox/outbox/history.
  - Primary evidence: protocol/core/server tests и real-account two-client integration.
  - Status: verified
  - Evidence: protocol/core/server and real-account host integration passed; workspace with voice-probe passed (two unchanged native opt-in ignores), focused actor3/contact4 tests passed after post-commit local-block cancellation. Fresh recursive-DNS signup/contact and genuine Invite/Ringing/Decline/no-microphone/teardown passed on Moto/Mi. This verifies account/crypto/route ownership, not the physical interval required by R2.
- R2: service-owned Android call lifecycle и настоящий GUI.
  - Source: утверждённый план, steps 6–7.
  - Acceptance: call из принятого чата, incoming notification/accept/decline, mute, endpoints/speaker, hangup; no microphone before answer; recreation/screen-off сохраняют активный call; teardown joins/releases audio/native/Telecom; one audio owner, bulk pause/resume, no stale replay after network loss.
  - Primary evidence: focused JVM tests и selected isolated `.gate` actual-client methods, including one finite 3–5min physical conversation on two authorised working phones.
  - Status: blocked
  - Evidence: shared audio, service/Core-Telecom owner and chat/call GUI implemented; JVM/debug/release/instrumentation builds passed. Genuine reject methods01/04 passed on both phones, with no microphone and complete teardown; accepted media/mute/unmute/recreation were observed but final default600 gates failed. No sustained3–5min, speaker, screen-off or conversational-quality PASS. Both approved parameter controls were exhausted and rolled back; no confirmed in-envelope code violation explains the delivery gap.
- R3: client release APK с прежней signing identity и публичным server profile.
  - Source: утверждённый план, step 8; user chose «Существующая подпись».
  - Acceptance: non-debuggable/minified `53.apk`, correct native/UniFFI/JNI, no probe Activity/JNI/fixtures or credentials; signing key read-only BuildKit secret, never image/context/cache layer; native16KiB alignment and APK≤25MiB.
  - Primary evidence: JVM/build gates, apksigner/manifest/native/asset inspection and selected signed-release device gate.
  - Status: verified
  - Evidence: current container release `53.apk`, SHA256 `efce8c2b6d894852e179331a5024cb04c65f76dbd3a5de9334856e9d2df487e0`, 9237963B, version3/0.5.0-53. apksigner verified existing certificate `19fcd9a9c3233ab14036bd54e9d75d04175007ea60abfede63ceab0df29c7acd`; non-debuggable, CallActivity present/probe Activity and JNI absent, expected public asset, zipalign-P16 and all ELF LOAD16KiB passed. Same-source signed/minified `.gate` release opened dialogs and accepted chat/call-button on Moto without identity reset, then stopped; this is startup acceptance, not R2 media acceptance.
- R4: authorised fresh backend rollout and scoped publication.
  - Source: user «fresh db без бекапов, нехуй плодить мусор, если fresh db всё упростит - апрув, на данные похуй»; «реализовать и коммит пуш».
  - Acceptance: only verified messenger DB/WAL/SHM/blob targets reset without new backup; source schema7 with preserved carrier/pins/topology/tunnel; fresh signup/contact/call/text smoke; intended source/docs committed/pushed, artifacts/private data not committed.
  - Primary evidence: selected deployment receipts, actual DNS client smoke, git status/diff/commit/remote equality.
  - Status: verified
  - Evidence: approved messenger-only fresh reset completed without new backup; initial image8eea22fd passed pong/schema7/invite_only with unchanged carrier. Current diagnostic msgd image7225cee582c5646388cc212682f74d520875e77149564a531e7b62562ca016d0 is healthy with the same normalized config/data mounts and restored default carrier. Fresh two-client DNS signup/contact/reject smoke and post-call TEXT receive/Delivered passed. Implementation committed/pushed as `82eb064566c92271c63033f3813c43f956a3c684`, HEAD equalled origin/master. Scoped deployed-source synchronization verified all3180 published files and removed exactly5 obsolete moved paths, without changing containers/config/secrets/volumes/vendor. Only the original unrelated design PNG remained untracked; APK/private artifacts were not committed.

### Constraints
- C1: frozen mono16k/12kbit/40ms/cap100, encoder10/decoder7, DTX/PLC; jitter80ms, sink640, JNI≤160,75% admission and all clock/packet-deadline validation remain unchanged. Probe baseline remains F0=600ms/ledger720ms/window2880B. Explicit 2026-10-12 permission permits one product-call F0=1200ms control (existing ledger1320ms/window5040B), retained only after short and3min actual-client PASS; otherwise restore600ms.
- C2: existing single process-owned NativeClient, independent Noise state per stream; no DNS/QUIC/CC/scheduler rewrite or automatic BBR/TCP_NODELAY experiment promotion. Explicit 2026-10-12 permission permits one existing-seam TCP_NODELAY control on messenger client/server and retention only if that control passes.
- C3: wire2/server7/core10, strict E2Ev2; no migration/legacy conversion/silent client wipe. Existing main8 data require explicit owner fresh onboarding; no phone main reset is implied by backend approval.
- C4: incoming only while normal «На связи» is connected; no instant Economy/force-stop/Doze promise. Transport loss ends current call; next dial uses fresh ID/key/counters.
- C5: secrets never logs/argv/env/Git/images, no audio files/STT; build tools clean allowlist. Existing lowered volume never raised. Pacman permission expired at19:30: no new operations without renewed approval.
- C6: preserve unrelated untracked design PNG and `.local`/builds/keys; current messenger backend data are expendable, carrier/noise identity and neighbouring internet tunnel are not.

### Non-goals
New codecs/FEC, 45min benchmarks/acoustic/STT frameworks, recording/call history, video/groups/hold/transfer, active-call autoreconnect, general retry/priority frameworks, firmware repair, old DB migration and unrelated deployment changes.

## Change Envelope
- Protocol call frames/key envelope; core shared media/ephemeral Olm/call actor/FFI/JNI and direct consumers; msgd volatile hub/authenticated attach/relay and affected ACL/egress gates.
- Android common audio, existing DmsgService/audio/bulk owners, Core-Telecom1.0.1/coroutines-android1.8.1, CallActivity/XML/localised resources/manifest; generated bindings only by codegen.
- Existing native/APK build/export and BuildKit secret packaging; goal/protocol/architecture/work-plan/deploy evidence. No new shared runtime crate or server codec dependency.
- Fresh rollout only to verified current messenger data mounts after implementation passes isolated gates; no new backup, volume prune/down-v or key rotation. The explicitly approved TCP_NODELAY control may recreate only the messenger carrier with unchanged revision/pins/config/data; neighbouring tunnel is excluded.

## Current Checkpoint
- Closed: R4/publication; substantive R2 work stopped at the approved change-envelope boundary.
- Evidence: implementation `82eb064` pushed and scoped deployed-source equality verified; publication does not prove R2 acceptance.
- Next action: none inside the exhausted approved controls. Do not repeat media/parameter controls or add a relay/transport redesign without new approval.
- Smallest unlock for R2: a working DNS media/feedback path inside the frozen limits, or a new explicit instruction permitting a concrete transport/feedback change. F0=1200 and TCP_NODELAY are not retained.

## Current State
- Resolved: shared media/ephemeral Olm/call actor/server/Android/packaging implemented; fresh backend deployed; authorised Moto/Mi gate accounts created without Main/Keystore resets; genuine Ring/Decline and teardown passed.
- Last relevant evidence: F0=1200ms short10s actual-client control passed both frameworks, including mute/unmute, recreation, receiver background, normal End and full teardown (Moto270encoded/237decoded, max feedback1039ms). The conditional3min control failed after101encoded packets: Moto oldest49661 age1320189us, 33entries/2160B, frontier49660, latest authenticated control age81527us; Mi reportedConnectionLost. This is not a3min PASS or causal fix. Product F0 restored600ms. Earlier LTE/Wi-Fi720ms and TCP_NODELAY controls remain negative; default carrier image32197dab restored, backend/data/pins/config unchanged. Moto Wi-Fi0/mobile_data1 restored; no experimental flag is retained.
- Blocker R2: latest default600 provenance gate failed with Mi oldest11814 age727642us>720000, frontier11813 and fresh authenticated control carrying that old frontier. Both profiles matched Android platform resolvers before/after reconnect/media, excluding fixture resolver retention for this run. Earlier server writes16 versus Mi receives4 and Full drops locate delivery gaps but not their sole cause. Changing intentional queue policy, transport or limits is not a proven local fix and lies outside the remaining approved controls.
- Verified current smoke: post-call TEXT received on Mi and Delivered on Moto via the existing retry/status path; minified release startup/chat call-button passed in isolated Moto `.gate`, no Main/Keystore reset. Root APK fully inspected; user-lowered volume unchanged, no audio files/STT.
- Published: `82eb064` on origin/master; deployed public source matches the published implementation, with5 obsolete moved files removed and container set unchanged. Remote `/opt/srv/53` is a source snapshot, not a Git checkout; its config/secrets/volumes/vendor were excluded from synchronization.
- Next: R2 unlock only; do not mark the overall goal complete or repeat unchanged failed gates.

## Material Decisions
- 2026-10-11: live mandatory; user replaces further M1-V qualification work with sealed-baseline client delivery. Missing latency/source-active/C_safe metrics remain unknown, not PASS.
- 2026-10-11: fresh messenger DB without new backups explicitly authorised; preserve pins, carrier, topology and neighbouring tunnel. No implicit main-device reset.
- 2026-10-11: three account-authenticated streams per endpoint (signal/RTP/SRTCP) over one native carrier. CallHub max2 calls/16 listeners, events≤4, media capacity1/hop20ms, ring30s/attach10s/heartbeat10s/liveness30s; no call SQL tables or persistent outbox.
- 2026-10-11: first client uses existing signing identity, non-debuggable release; not final production signing policy.
- 2026-10-12: source calibration's accumulated bit4 also records stationary position/new timestamp; production now distinguishes actual frame/time reversal from a rejected calibration sample. Calibration/frequency guards and all numeric bounds are unchanged; focused clock/route JVM checks passed.
- 2026-10-12: user chose «Разрешаю TCP_NODELAY»: one short existing-seam messenger client/server control, including messenger-only carrier recreate, and retention for client release if it passes. No pin/revision/DB reset/DNS/QUIC/CC/scheduler or neighbouring-tunnel change is authorised.
- 2026-10-12: after TCP_NODELAY negative/rollback user chose «Разрешаю контроль F0»: one existing product config F0=1200ms experiment, retaining it only after both short and3min physical gates. This explicitly changes only product ledger tolerance/window; no probe/jitter/sink/codec/packet-deadline/relay/transport change.

## Checkpoint History
- 2026-10-12: host/build/fresh-rollout/signup/contact checkpoint passed. Real reject/no-microphone/teardown passed after fixing initial Listening availability, pre-ID notification ownership and Core-Telecom collector lifetime. Accepted short media remains negative (ledger expiry above); no long-call or release-device PASS is claimed.
- 2026-10-12: one approved existing-seam TCP_NODELAY control built without probe JNI and recreated only messenger carrier (same config/pins/revision). Ringing→Expired preceded media; both framework gates failed. Restored default carrier/native and removed temporary build switches; no performance or causal PASS is inferred.
- 2026-10-12: approved product F0=1200ms/ledger1320ms control passed10s but failed conditional3min at age1320189us; both final frameworks negative on that run. Restored source and both phone binaries to F0=600ms, no probe/codec/packet-deadline/transport change. Current-source release rebuilt.
- 2026-10-12: finite default600ms server-counter discriminator failed: side1/RTP complete TCP writes16 versus Mi authenticated receive4; Full drops occurred (side1 RTP7/SRTCP1, side0 RTP1/SRTCP1), pre-Noise and route-aged expiry0. Counts prove socket acceptance and bounded queue outcomes only, not downstream delivery or sole cause. Diagnostic backend deployed without data reset; next check resolver provenance, not transport/queue redesign.
- 2026-10-12: default600 resolver-provenance check failed media but both saved profiles matched Android platform DNS throughout. APK static/signature/native/profile checks, minified `.gate` startup/chat button and post-call TEXT/Delivered passed. Stop R2 work at the approved boundary and publish without claiming sustained phone conversation.
- 2026-10-12: implementation committed/pushed as `82eb064`; scoped source snapshot synchronized on the deployment (3180 file hashes verified, exactly5 obsolete moved files removed, container set unchanged). R4 resolved; R2 remains blocked, with no new physical acceptance inferred from publication.

## Completion
- Resolved outcomes: R1/R3/R4 verified; R2 blocked by the documented actual-client ledger expiry and exhausted approved controls.
- Commands/artifacts: host/JVM/native/build checks and selected DNS signup/contact/reject/TEXT gates above; signed `53.apk` (SHA256 `efce8c2b6d894852e179331a5024cb04c65f76dbd3a5de9334856e9d2df487e0`, 9237963B); implementation `82eb064` pushed and deployed-source equality verified.
- Constraint/scope check: final product/probe F0=600ms, ledger720ms/window2880B; no TCP_NODELAY promotion, clock/codec/jitter/sink/deadline relaxation, Main/Pacman reset, audio files/STT or unrelated tunnel change. Original untracked design PNG preserved.
- Final status: blocked, not complete. No claim of full M1-V qualification, sustained DNS conversation or Mi firmware repair. Smallest unlock is the R2 instruction/state change recorded above.
