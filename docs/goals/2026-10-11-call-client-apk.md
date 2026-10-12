# Goal: клиентские аудиозвонки и подписанный APK

Status: active
Source: план «контакт → настоящий звонок → завершение → клиентский APK», утверждённый пользователем 2026-10-11; «реализовать и коммит пуш». 2026-10-12: полностью разрешено патчить Slipstream для DNS-связи; «Делай копию плана в цель и итеративно реализовать» и снять прежние transport-source запреты.
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
  - Status: in_progress
  - Evidence: shared audio, service/Core-Telecom owner and chat/call GUI implemented; JVM/debug/release/instrumentation builds passed. Genuine reject methods01/04 passed on both phones, with no microphone and complete teardown; accepted media/mute/unmute/recreation were observed but final default600 gates failed. No sustained3–5min, speaker, screen-off or conversational-quality PASS. Parameter controls were negative and rolled back. Read-only source review found a concrete prepared-packet discard in Slipstream; causal relation to the physical failure is not yet verified. New source-edit permission reopens R2.
- R3: client release APK с прежней signing identity и публичным server profile.
  - Source: утверждённый план, step 8; user chose «Существующая подпись».
  - Acceptance: non-debuggable/minified `53.apk`, correct native/UniFFI/JNI, no probe Activity/JNI/fixtures or credentials; signing key read-only BuildKit secret, never image/context/cache layer; native16KiB alignment and APK≤25MiB.
  - Primary evidence: JVM/build gates, apksigner/manifest/native/asset inspection and selected signed-release device gate.
  - Status: in_progress
  - Evidence: previous baseline container release `53.apk`, SHA256 `efce8c2b6d894852e179331a5024cb04c65f76dbd3a5de9334856e9d2df487e0`, 9237963B, version3/0.5.0-53, passed signature/native/profile and minified startup checks. Existing certificate is `19fcd9a9c3233ab14036bd54e9d75d04175007ea60abfede63ceab0df29c7acd`. The transport update requires a newly built and verified APK; old startup acceptance is not R2 media acceptance.
- R4: authorised fresh backend rollout and scoped publication.
  - Source: user «fresh db без бекапов, нехуй плодить мусор, если fresh db всё упростит - апрув, на данные похуй»; «реализовать и коммит пуш».
  - Acceptance: only verified messenger DB/WAL/SHM/blob targets reset without new backup; source schema7 with preserved carrier/pins/topology/tunnel; fresh signup/contact/call/text smoke; intended source/docs committed/pushed, artifacts/private data not committed.
  - Primary evidence: selected deployment receipts, actual DNS client smoke, git status/diff/commit/remote equality.
  - Status: in_progress
  - Evidence: approved messenger-only fresh reset completed without new backup; initial image8eea22fd passed pong/schema7/invite_only with unchanged carrier. Current diagnostic msgd image7225cee582c5646388cc212682f74d520875e77149564a531e7b62562ca016d0 is healthy with the same normalized config/data mounts and restored default carrier. Fresh two-client DNS signup/contact/reject smoke and post-call TEXT receive/Delivered passed. Implementation committed/pushed as `82eb064566c92271c63033f3813c43f956a3c684`, HEAD equalled origin/master. Scoped deployed-source synchronization verified all3180 published files and removed exactly5 obsolete moved paths, without changing containers/config/secrets/volumes/vendor. Only the original unrelated design PNG remained untracked; APK/private artifacts were not committed.

### Constraints
- C1: frozen mono16k/12kbit/40ms/cap100, encoder10/decoder7, DTX/PLC; jitter80ms, sink640, JNI≤160,75% admission and all clock/packet-deadline validation remain unchanged. Product/probe F0=600ms/ledger720ms/window2880B. The earlier approved F0=1200ms control failed its3min gate and was restored; this transport iteration does not repeat it.
- C2: existing single process-owned NativeClient, independent Noise state per stream. User explicitly permits editing Slipstream sources, including DNS/QUIC/congestion/scheduler, to fix DNS connectivity; former source-edit prohibitions are removed. Client/carrier share one pinned Git revision, embedding stays separate from messenger logic, crypto/pins remain intact. First selected fix is prepared-packet submission, not automatic BBR/TCP_NODELAY promotion.
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
- Slipstream C sources/tests and transport Git history, root submodule gitlink/branch, native PIN checks/staging and documented messenger-only carrier rollout. The old research checkout `.local/slipstream` is not edited. Source fixes may change the pinned revision, not pins/secrets/schema/media bounds.
- Prior fresh rollout is complete; this transport iteration requires no new data reset/backup, volume prune/down-v or key rotation. Recreate only messenger carrier after gates, preserve config/data/backend, exclude neighbouring tunnel.

## Current Checkpoint
- Closes: R2; then refresh R3/R4 for the new transport revision.
- Completed: prepared-packet regression/fix, normal/sanitized Meson suites, embedded lifecycle/pin/loopback, ARM64 and matching gate builds passed at62b9649. Full workspace passed423 tests with2 existing opt-in ignores. Two finite phone observations still failed; no180s run or carrier promotion followed.
- Smallest next action: localize the remaining covering-SRTCP delivery gap between peer preparation/commit, relay socket acceptance and authenticated source receipt. Independent final snapshots cannot prove timely delivery; do not replace this with another unchanged call loop.
- Expected evidence: one specific covering report traced through the existing bounded owners, or another source-confirmed violation reachable in this low-rate call. Allocation-failure-only credit findings are not a demonstrated cause and are outside this checkpoint.
- Stop/replan if: the source fix passes but physical media still expires. Keep failed evidence, do not raise F0 or run unchanged long loops; use existing counters to locate the remaining path.

## Current State
- Resolved: shared media/ephemeral Olm/call actor/server/Android/packaging implemented; fresh backend deployed; authorised Moto/Mi gate accounts created without Main/Keystore resets; genuine Ring/Decline and teardown passed.
- Transport source: `62b964998be3c3d7fd1bee2195547613d053b202` committed/pushed to `0FL01/slipstream:fix/dns-prepared-send`; parent gitlink/PIN checks updated. Picoquic/SPCDNS gitlinks and overlay/default options unchanged. Embedded lifecycle and explicit loopback passed; production ARM64 and gate builds passed and were installed only on authorised Moto/Mi without data reset. Release/carrier promotion remains pending after failed physical gates.
- Last relevant evidence: F0=1200ms short10s actual-client control passed both frameworks, including mute/unmute, recreation, receiver background, normal End and full teardown (Moto270encoded/237decoded, max feedback1039ms). The conditional3min control failed after101encoded packets: Moto oldest49661 age1320189us, 33entries/2160B, frontier49660, latest authenticated control age81527us; Mi reportedConnectionLost. This is not a3min PASS or causal fix. Product F0 restored600ms. Earlier LTE/Wi-Fi720ms and TCP_NODELAY controls remain negative; default carrier image32197dab restored, backend/data/pins/config unchanged. Moto Wi-Fi0/mobile_data1 restored; no experimental flag is retained.
- Unresolved R2: new-client/old-carrier10s gate failed; the first observation reached Moto111encoded/85decoded/105received before control-lane failure. A bounded follow-up with final/DNS-state reporting identified Moto ledger expiry: oldest5201 age720356us>720000,19entries/1011B, frontier5200; last authenticated terminal5200 was407850us old. Mi independently ended with nativefailed=false and validated5219/prepared+committed5218. This does not prove a covering report arrived before the expiry. Saved profiles matched Android platform resolvers. No180s/speaker/screen-off/conversation PASS; the confirmed discard fix is not sufficient by itself.
- Verified current smoke: post-call TEXT received on Mi and Delivered on Moto via the existing retry/status path; minified release startup/chat call-button passed in isolated Moto `.gate`, no Main/Keystore reset. Root APK fully inspected; user-lowered volume unchanged, no audio files/STT.
- Published: `82eb064` on origin/master; deployed public source matches the published implementation, with5 obsolete moved files removed and container set unchanged. Remote `/opt/srv/53` is a source snapshot, not a Git checkout; its config/secrets/volumes/vendor were excluded from synchronization.
- Next: publish this verified transport-fix checkpoint, then localize the remaining feedback path. Production carrier/backend and root version3 APK remain the old baseline; no unpassed transport release is promoted.

## Material Decisions
- 2026-10-11: live mandatory; user replaces further M1-V qualification work with sealed-baseline client delivery. Missing latency/source-active/C_safe metrics remain unknown, not PASS.
- 2026-10-11: fresh messenger DB without new backups explicitly authorised; preserve pins, carrier, topology and neighbouring tunnel. No implicit main-device reset.
- 2026-10-11: three account-authenticated streams per endpoint (signal/RTP/SRTCP) over one native carrier. CallHub max2 calls/16 listeners, events≤4, media capacity1/hop20ms, ring30s/attach10s/heartbeat10s/liveness30s; no call SQL tables or persistent outbox.
- 2026-10-11: first client uses existing signing identity, non-debuggable release; not final production signing policy.
- 2026-10-12: source calibration's accumulated bit4 also records stationary position/new timestamp; production now distinguishes actual frame/time reversal from a rejected calibration sample. Calibration/frequency guards and all numeric bounds are unchanged; focused clock/route JVM checks passed.
- 2026-10-12: user chose «Разрешаю TCP_NODELAY»: one short existing-seam messenger client/server control, including messenger-only carrier recreate, and retention for client release if it passes. No pin/revision/DB reset/DNS/QUIC/CC/scheduler or neighbouring-tunnel change is authorised.
- 2026-10-12: after TCP_NODELAY negative/rollback user chose «Разрешаю контроль F0»: one existing product config F0=1200ms experiment, retaining it only after both short and3min physical gates. This explicitly changes only product ledger tolerance/window; no probe/jitter/sink/codec/packet-deadline/relay/transport change.
- 2026-10-12: user fully authorises editing Slipstream sources to solve DNS connectivity and explicitly requests copying the transport plan into this goal and iterative implementation. This supersedes prior DNS/QUIC/congestion/scheduler/vendor-source prohibitions; no neighbouring-tunnel, key/pin, data reset or media-bound change is implied.

## Checkpoint History
- 2026-10-12: host/build/fresh-rollout/signup/contact checkpoint passed. Real reject/no-microphone/teardown passed after fixing initial Listening availability, pre-ID notification ownership and Core-Telecom collector lifetime. Accepted short media remains negative (ledger expiry above); no long-call or release-device PASS is claimed.
- 2026-10-12: one approved existing-seam TCP_NODELAY control built without probe JNI and recreated only messenger carrier (same config/pins/revision). Ringing→Expired preceded media; both framework gates failed. Restored default carrier/native and removed temporary build switches; no performance or causal PASS is inferred.
- 2026-10-12: approved product F0=1200ms/ledger1320ms control passed10s but failed conditional3min at age1320189us; both final frameworks negative on that run. Restored source and both phone binaries to F0=600ms, no probe/codec/packet-deadline/transport change. Current-source release rebuilt.
- 2026-10-12: finite default600ms server-counter discriminator failed: side1/RTP complete TCP writes16 versus Mi authenticated receive4; Full drops occurred (side1 RTP7/SRTCP1, side0 RTP1/SRTCP1), pre-Noise and route-aged expiry0. Counts prove socket acceptance and bounded queue outcomes only, not downstream delivery or sole cause. Diagnostic backend deployed without data reset; next check resolver provenance, not transport/queue redesign.
- 2026-10-12: default600 resolver-provenance check failed media but both saved profiles matched Android platform DNS throughout. APK static/signature/native/profile checks, minified `.gate` startup/chat button and post-call TEXT/Delivered passed. Stop R2 work at the approved boundary and publish without claiming sustained phone conversation.
- 2026-10-12: implementation committed/pushed as `82eb064`; scoped source snapshot synchronized on the deployment (3180 file hashes verified, exactly5 obsolete moved files removed, container set unchanged). R4 resolved; R2 remains blocked, with no new physical acceptance inferred from publication.
- 2026-10-12: copied transport plan/permissions recorded. In isolated worktree fromd7cd555, actual-loop `poll-stale-flag` regression failed old code (SIGABRT before encode), then passed the9-line submission/cleanup fix. Meson protocol/path/pin/poll-submit/runtime5/5 passed; runtime16tests includes new three-stream low-rate case (also passed baseline, not a performance improvement proof). Clang ASan/UBSan all5 suites passed. Root/vendor and deployed services not changed in this checkpoint; actual Android causality remains open.
- 2026-10-12: reviewed four-file transport diff and published commit62b9649 via existing SSH agent after HTTPS push lacked interactive credentials. Updated root gitlink/branch and all active sys PIN checks to the same revision; no Git config, nested dependency revision, deployment or phone state changed by publication.
- 2026-10-12: native lifecycle/default-off seams and explicit pinned-server loopback passed; ARM64 production build35.96s and gate build13s passed. New clients on Moto/Mi versus unchanged deployed carrier failed the10s gate. Existing server summaries showed RTP Full drops5/8 and SRTCP Full0/1, no pre-Noise/route expiry; complete socket writes remain distinct from delivery. Test-only final state/DNS reporting exposed the remaining720356us expiry. No timers/queues/F0 changes, long run, carrier/backend recreate, Main/Keystore reset, audio files or volume increase.
- 2026-10-12: `cargo build -p msgd` and `cargo test --workspace --features dmsg-core/voice-probe -- --test-threads=2` passed at the new pin (423 passed,2 unchanged native opt-in ignores; explicit slipstream-sys loopback separately passed). Core/auth/TEXT/Olm/VOICE and media regression checks are green, not physical conversation acceptance.

## Completion
- Resolved outcomes: R1 remains verified; R2/R3/R4 reopened for the explicitly approved transport iteration. Prior publication/build evidence remains historical and is not new transport acceptance.
- Commands/artifacts: host/JVM/native/build checks and selected DNS signup/contact/reject/TEXT gates above; signed `53.apk` (SHA256 `efce8c2b6d894852e179331a5024cb04c65f76dbd3a5de9334856e9d2df487e0`, 9237963B); implementation `82eb064` pushed and deployed-source equality verified.
- Constraint/scope check: final product/probe F0=600ms, ledger720ms/window2880B; no TCP_NODELAY promotion, clock/codec/jitter/sink/deadline relaxation, Main/Pacman reset, audio files/STT or unrelated tunnel change. Original untracked design PNG preserved.
- Final status: active, not complete. No claim of full M1-V qualification, sustained DNS conversation or Mi firmware repair.

## Копия утверждённого transport-плана, 2026-10-12

### Цель
Исправить потерю уже подготовленных QUIC-пакетов внутри Slipstream и закрыть оставшийся клиентский gate: устойчивый двусторонний DNS-звонок3–5min, без увеличения ledger/jitter/audio queues и без регрессии TEXT/VOICE. Патчим отдельное развёртывание мессенджера; соседний интернет-туннель не затрагиваем.

### Критерии done
- Подготовленный ненулевой QUIC-пакет не отбрасывается потому, что вместо POLL содержит данные/ACK; encode error не маскируется продолжением transport run.
- Проходят regression, protocol/path/pin/runtime, native lifecycle и relevant Core/Android tests.
- Два настоящих клиента выдерживают180s через recursive DNS: двусторонний звук, mute/unmute, recreation, background/screen-off, speaker, нормальный hangup.
- После call проходят TEXT и voice-note transfer; client/carrier собираются из одной новой Git-ревизии.
- Обновлённый signed release APK сохраняет signing identity, accounts/Keystore/server data/pins.

### Текущее устройство и выбранный дефект
`src/slipstream_sockloop.c:288–307` устанавливает `is_poll_requested`, вызывает `picoquic_prepare_packet_ex`, затем при оставшемся флаге очищает его и делает `break` без отправки ненулевого пакета. Picoquic оставляет флаг при подготовке обычных data/ACK frames, но уже выделил packet number, поставил packet в retransmission queue и учёл bytes_in_transit/pacing (`sender.c::picoquic_finalize_and_protect_packet`, `picoquic_queue_for_retransmit`). Это подтверждённый source defect, не доказанная единственная причина physical ledger expiry.

Embedded Android client и CLI/server используют общий sockloop; исходный baseline PIN `d7cd5555a88933053551128ff8b3741ae93049a0` (новая ревизия записана в Current State). Native staging экспортирует Git objects и сохраняет embedding/picoquic patches. RTP40ms/SRTCP200ms держат active call в BUSY: quiet polling2s не выбран причиной и не меняется. Server TCP acceptance не доказывает доставку телефону.

### Изменения по порядку
1. **Разрешение/цель:** записать source-edit permission в goal/AGENTS/ARCHITECTURE, перевести цель в active/R2 in_progress. Сохранить отрицательные parameter controls и unknown quality metrics; не разрешать data wipe/pin change/соседний tunnel.
2. **C packet submission:** в transport `src/slipstream_sockloop.c` очищать принадлежащий вызову poll-request после prepare; оставить terminal prepare error и no-send при zero length; при positive length encode/send независимо от POLL presence. Sent counter растёт только после successful UDP submission. В обоих client send paths positive prepared packet + encode<=0 освобождает owned allocation и возвращает error вместо continue; освобождать allocation перед существующими early encode-error returns. Server empty/error DNS replies не менять. Никаких manual bytes_in_transit rollback/packet-number reuse/state reset/new queue/retry/feature flags.
3. **Regression:** новый `tests/test_poll_submit.c` вызывает фактический packet loop, подменяя prepare/select/encode/send только в test translation unit, без production callback framework. Проверить positive packet with cleared/still-set POLL flag (ровно1 send), zero packet(no encode/send), prepare error(no stale request), regular/poll encode failure(terminal/no false sent/leak), sparse sent-vs-empty deadline. Сначала stale-flag case должен падать на старом коде, затем пройти. В existing runtime fixture добавить3 low-rate streams с40ms/200ms/rare control, exact sequence/payload, idle→active transition; loopback не считать recursive acceptance.
4. **Одна Git-ревизия:** отдельный transport worktree от PIN, branch `fix/dns-prepared-send`, commit/push; `.local/slipstream` не редактировать. Обновить root gitlink/branch, `native/stage.py`, PIN constants в2 regression scripts и sys README. Picoquic/SPCDNS revisions, embedding patch, default-OFF TCP_NODELAY неизменны; C ABI/bindings/JNI API не меняются.
5. **Проверки:** `cargo test -p slipstream-sys`, `python3 crates/slipstream-sys/tests/run-native-gates.py`, existing Meson release/ASan/UBSan, `cargo build -p msgd && cargo test --workspace`, production ARM64 native и Android JVM/debug/release builds. Старые артефакты сохранить для сравнения. На разрешённых `.gate` phones установить matching target/test без uninstall/reset; client02/client03 сначала10s, послеPASS180s с речью. В full180s сценарии добавить speaker через Telecom и screen-off/wake; short diagnostic без этих действий. Проверить reject/no-microphone, post-call TEXT05/06 и voice round-trip/resume. Formal results + existing counters only; без PCM/credentials/payload capture и повышения пользовательской громкости. При short failure не запускать ещё один long loop/повышениеF0: записать оставшийся участок задержки.
6. **Rollout/APK:** сначала new client со старым carrier (wire format прежний). После gates собрать carrier из того же нового SHA, сохранить old image digest, recreate только messenger slipstream без msgd restart/reset/cert/Noise/reset-seed/ports/topology changes; normalized config metadata equality и bounded post-rollout call. Финальный APK versionCode4, прежняя подпись/public profile/probe-OFF native; повторить signature/nondebug/R8/16KiB/native/profile/size/minified-startup checks. Carrier rollback на old image; published nondebug APK rollback только higher-version APK, не uninstall.

### Сохраняемые проверки и риски
Pin/signature rejection, forced-path identity/compaction, partial writes/half-close, multipath loss/bootstrap fallback, quiet/empty poll bounds, single-owner/stop/join/FD/task/signals, auth/TEXT/Olm/call crypto/relay/voice resume остаются зелёными. Primary acceptance — реальный180s conversation, не echo/write counts/timeout growth.

Discard может быть одной из нескольких причин; локальный fix не гарантирует работу через любой filtering/rate-limited resolver. F0=600/ledger720/window2880/jitter80/sink640 сохраняются. Mi HAL failure не выдавать за transport result; Pacman без нового разрешения не использовать. Нет migrations/identity resets/new backups.

### Вне первого патча
CC/POLL wire semantics/quiet intervals/MTU/stream priorities/relay queues не менять без следующего доказанного checkpoint. Отдельный server UDP-error finding не добавлять без связи с текущим сбоем; не переписывать весь transport и не обещать универсальное устранение DNS-фильтрации.
