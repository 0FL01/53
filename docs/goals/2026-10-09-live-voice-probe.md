# Goal: M1-V — защищённый live voice probe через узкий DNS tunnel

Status: active
Source: пользователь 2026-10-09: аудит media-плана, затем «делай копию плана в цель и итеративно реализовать и коммит пуш, так же телефон для тестов использовать»; дополнение «Вот тебе два телефона ... блокер снят»: разрешены Moto ZY22JFJ5LP и Pacman 192.168.1.55:39631. ARCHITECTURE §7–9; WORK_PLAN M1-V, не M6.
Last updated: 2026-10-09

## Objective
Реализовать минимальный memory-only двусторонний Opus/RTP/SRTP probe поверх закреплённого Slipstream DNS и получить воспроизводимые измерения delivery/latency/late audio/очередей. Это эксперимент пригодности канала, не продуктовая функция звонков. Upload 50–80 kbit/s — **полезная скорость внутри tunnel на каждом endpoint**, downlink 1–2 Mbit/s; внешний канал быстрее.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Stop substantive work at a proven external blocker, an approved budget boundary, or when no remaining in-scope action has a falsifiable expected result; record the exact evidence and smallest unlock.

## Frozen Contract

### Required Outcomes
- R1: Исправленный план и durable execution state в цели; итеративные scoped commits и push.
  - Source: последний запрос пользователя.
  - Acceptance: этот документ содержит замороженный план, evidence и следующий checkpoint; intended commits опубликованы в origin, без unrelated PNG/secrets/.local/build artifacts.
  - Primary evidence: goal, git status/diff/log и успешные scoped push.
  - Status: verified
  - Evidence: goal7a37e47, codec4bc5df8, SRTP9bb9ecb, probe aeae648, Android d4fec90 и readiness/evidence e1b0f4a опубликованы в origin/master scoped commits. Прежняя cc588da blocked closure возобновлена после разрешения второго телефона. Clean-env SSH push использует штатный SSH_AUTH_SOCK; build/test его не получают. Пользовательский untracked PNG сохранён, secrets/.local/build artifacts не staged.
- R2: Отдельный bounded live Opus API, без изменения voice-note контракта.
  - Source: аудированный media-план, ARCHITECTURE §9, переиспользование голосовых сообщений из исходного запроса.
  - Acceptance: исходный кандидат mono PCM16/16kHz, VOIP, 12 kbit/s, 40ms/640samples, VBR с реальным max_data_bytes=100, encoder10/decoder7, WB maximum/AUTO, DTX/PLC. FEC/DRED/BWE/QEXT off. Legal tiny DTX packets допустимы, zero-byte incoming packet запрещён; duration/mono/bandwidth/cap проверяются. Existing 10k/20ms DVN API/validation неизменны. NoLACE и Deep PLC проверяются по actual decoder path, не только CTL readback.
  - Primary evidence: live codec tests, existing voice_codec/voice-note regressions, host/Android native linkage.
  - Status: verified
  - Evidence: host live6 + note-container6/core voice_codec6 PASS, note contracts unchanged. Exact packagedLiveCodecActivationOnlyInGatePackage on Moto/API35 PASS: actual arm64 WB40 packets100, NoLACE changed31998samples, isolated Deep PLC4/5 changed1907samples, lookahead104. Fresh isolated target avoids old unrelated CMake cache; no privileged/upstream/config changes.
- R3: Memory-only protected duplex media/feedback/relay с ограниченными очередями и retirement старой generation.
  - Source: аудированный media-план; ARCHITECTURE §7–9.
  - Acceptance: один bidirectional media lane и один control lane на endpoint, независимые Noise states; RTP/SRTP AES_CM_128_HMAC_SHA1_80, SRTCP feedback. Один pending plaintext media packet + один committed frame; deadline drop до Noise, после commitment — finish ordered frame либо уничтожить lane. Remote-authenticated terminal progress ограничивает admitted bytes; local writes не считаются ACK. Relay не получает media keys и не декодирует звук. Fresh generation/Noise/SRTP keys/SSRC/codec/queues после coordinated fixture restart; старый material не replay.
  - Primary evidence: protected duplex/crypto/parser/queue/credit/generation tests и two-endpoint probe results.
  - Status: verified
  - Evidence: SRTP9bb9ecb/probe aeae648 pushed; SRTP host12/arm64 and integrated probe16 PASS. Workspace271 PASS, two existing native tests ignored unchanged; fmt PASS. Source-scoped controlled LAN DNS phone↔host traversed two actual pinned C clients, independent Noise lanes, SRTP/SRTCP and opaque relay. Fresh epoch after coordinated retirement kept fixture Noise trust identities; phone NativeClient stop/join1–8ms, host25–42ms. Actual readiness/carrier flags required; no loopback/USB substitution or production/per-lane reconnect claim.
- R4: Реальный foreground Android audio probe на разрешённом телефоне.
  - Source: последний запрос, аудированный Android atom; AGENTS.md/AUTH_GATES.md.
  - Acceptance: visible target-UID host только .gate, explicit mic permission, real AudioRecord/AudioTrack, bounded 10ms JNI batches и independent capture/render/media progress; VOICE_COMMUNICATION/focus/effects/route observations. Playback queue учтена в deadlines, clock correction измеряется, lifecycle cleanup/cancel не зависит от DB/Olm. Main/Pacman/их identity/data не изменяются.
  - Primary evidence: selected exact-method .gate instrumentation на явно разрешённых Moto/Pacman, arm64/JVM/debug/release builds и real mic/playback/cleanup evidence.
  - Status: verified
  - Evidence: arm64/API26 native and JVM122/debug/release/androidTest builds PASS. Exact foregroundMicrophoneProtectedLoopbackAndCleanupOnlyInGatePackage on Moto/API35 PASS: real mic/playback, enabled AEC/NS, actual640sample sink bound, hardware timestamps and16008Hz actuator slope16007.7066Hz, pause cleanup/stop5ms. Final8s running baseline decoded201/PLC0/late0/captureDrop0/renderDrop0/underrun0; intentional teardown separated. DNS exact-method establishes R3 plumbing, not R5 physical quality. Main/identities untouched.
- R5: Честный сквозной DNS/voice experiment и решение по профилю.
  - Source: исходный запрос/исследование, ARCHITECTURE §9.173–177, WORK_PLAN M1-V.
  - Acceptance: timestamp screening и duplex audio по actual DNS, полезная граница byte accounting явно указана; controlled 50/80 application service model отделена от observed DNS service rate. Для полной physical приёмки — два реальных Android, оба upload активны, ≥30min (план 45min) выбранного профиля; common-clock mouth-to-ear в обоих направлениях, late active duration включает pre-send drops, stalls/queue drift/bytes/query rate/ресурсы/понятность. Не заменять physical pair одним phone+host, USB/DirectTCP либо RTT/2. Нормальная цель p95≤400ms/late≤2%; 700ms/5% только кандидат экспериментального пилота, не low-latency обещание. Негативный результат сохраняется как результат, не маскируется изменением gate.
  - Primary evidence: sanitized run summaries, exact topology/config/build versions, source/sink timing и профильное решение с границами evidence.
  - Status: in_progress
  - Evidence: оба явно разрешённых физических Android подтверждены: Moto/API35 и Pacman/API36. Детерминированные timing regressions исправлены, focused probe26 PASS; local8s на обоих — active PLC/late/renderDrop0. Первый actual two-phone direct-authoritative DNS20s/40ms: Moto RTP498/decoded411/late87/PLC87, Pacman RTP500/decoded500/late0/PLC0; actual render expiry160/480 samples отдельно учтена, topology/cleanup PASS. Все87 late Moto прибыли после nominal due, не преждевременный PLC. Python-forwarder стенд отдельно отказал на EAGAIN; обход observer исключает этот confound. Declared admission50k/F0=600ms не считать measured DNS capacity; mouth-to-ear не измерен, качество не объявлено PASS. Продолжать доступную причинную диагностику.

### Constraints
- Закреплённый vendor/slipstream d7cd5555a88933053551128ff8b3741ae93049a0; не править DNS/QUIC/congestion/scheduler, vendor/.local исследовательский checkout или working deployment.
- Один native carrier на endpoint; fixture probe отказывает при занятом runtime, не останавливает чужой owner. Никаких обязательных HTTPS/STUN/TURN/FCM или server transcoding.
- Секреты только owner-only/read-only private files, не Git/argv/env/logs. Relay получает лишь public routing/trust descriptor, не SRTP keys. Production audio не сохраняется; запись разрешённого test material — отдельная измерительная fixture.
- Fresh server7/core10/wire2/E2Ev2 и Main8/backend5 остаются прежними: никаких DB/schema/auth/message-format changes, migration/autowipe или production rollout.
- Build/diagnostics — clean allowlist environment. External disposable harness имеет отдельный CARGO_TARGET_DIR, не перетирает host codegen artifacts.
- Только ZY22JFJ5LP и 192.168.1.55:39631, .gate/.gate.test и exact-method am instrument. Gate install разрешён на обоих, Main reset/install/identity changes — нет. Не stage исходный unrelated design PNG.

### Non-goals
- Production call signaling/Olm key exchange, history/UI/Telecom/FGS/background/lockscreen/incoming calls, schema11, новое msgd call API.
- Lyra/BWE/FEC/DRED, автоматические bitrate/duration tiers, custom AEC/resampler/VAD/CNG, generic actor/retry/capability framework.
- Native telemetry ABI и shared per-stream RST bridge patch до доказанной необходимости; 2/4 lanes и eight-pair capacity acceptance не включаются в первый gate.

## Change Envelope
- crates/opus-sys: отдельный live codec API/tests, existing note types неизменны.
- Минимальный statically bundled crates/srtp-sys (libsrtp2.7.0, C shim/owned state/tests), Cargo workspace/core dependencies и обязательные workspace-copy/native build consumers. Это реальная native dependency boundary, не новый сервис.
- crates/core: небольшой probe module с fixture-local DTO/duplex Noise/media/feedback и memory-only example relay; focused tests; Android-only narrow probe JNI. No store/chat/history/account-auth changes.
- android: debug/.gate-only visible probe host/audio/test methods/manifest permissions, native build feature/argument; без production call UI и ручного редактирования UniFFI bindings.
- Goal и непосредственно затронутые build/gate инструкции; ignored private disposable fixtures/owned isolated DNS test topology разрешены, production containers/ports/data — нет.

## Copied corrected implementation plan
1. **Live codec отдельно от notes.** Сначала immutable per-run 12k/40ms/cap100 профиль. Validate полный Opus packet, не count==1/nonempty coded frame из DVN; explicit exact-duration PLC. Tiny non-DTX fallback при слишком тесном cap считать потерей speech, не успешным quality result. OSCE уже компилирует Deep PLC; не добавлять фиктивный build flag. Активация NoLACE WB40 и отдельный Deep PLC 4/5 A/B; existing notes regression.
2. **Fixture-only protected probe.** Read-only descriptor задаёт epoch, Noise pins/identities, fresh directional SRTP keys/salts и SSRC. Relay descriptor без media keys. Это не production E2E key exchange. Один ordered media path; own Noise owner на lane, partial reads/writes сохранены. libsrtp standard protection/replay/rollover; DTO остаётся private probe, protocol/message formats не меняются.
3. **Feedback и admission.** Private pre-agreed M1V RTCP profile: SR-or-RR + SDES(8 printable CNAME octets) + APP(32B body), whole compound защищён SRTCP, nominal каждые200ms, missed ticks coalesce. Нет reduced-size/SDP/AVPF negotiation claim. Lanes привязаны к epoch при setup, без per-packet outer call/epoch. SR-case 152 framed bytes (=6.08kbit/s), RR-case132. Media среднее20.8/cap28.8kbit/s; с feedback26.88/34.88. 75% от измеренного safe useful capacity — стартовый экспериментальный admission budget, не гарантия TEXT bandwidth. APP сообщает authenticated terminal RTP index и retired playout cursor (не hardware ACK), late/PLC и mute/DTX snapshots. Credit освобождается terminal frontier только на одном ordered path; source ledger хранит index/bytes/commit time. Freeze healthy feedback cycle F0, Wmax=R_media_cap*(F0+0.120)/8+2Pmax; не расширять вслед за backlog. Send all returned legal DTX packets в первом baseline; suppression/custom CNG отложены.
4. **Bounded playout и fixture recovery.** Один waiting media packet, soft20/hard40ms после encode; pacing без catch-up bursts, receiver auth до stale-drop, не rewind после PLC. Fixed80ms jitter baseline, encoded future queue≤200ms, actual AudioTrack queued PCM входит в presentation deadline. Slow occupancy/audio-timestamp correction отдельно от network delay, freeze на unhealthy progress/route discontinuity; setPlaybackRate как измеряемый platform actuator, ±500ppm относительного source-to-sink skew (integer rate quantization учитывать). Без custom DSP/quantile jitter adaptation до результата baseline. Recovery retire generation у обоих peers/relay, stop producers/audio, cancel/drop all lanes/queues, stop/join existing isolated NativeClient, затем fresh carriers/Noise/keys/SSRC/codec. Stop/join измеряется, 50ms wait cap не обещает total teardown time. One coordinated fixture restart per failure scenario; 10s observer timeout означает failed test, не разрешение открыть второй carrier поверх незавершённого первого. Per-lane abort/sibling preservation и production reconnect этим не доказаны.
5. **Foreground Android probe.** Visible target-UID host только .gate, permission/focus/communication mode с cleanup, отдельные bounded capture/render workers, 10ms JNI. Necessary MODIFY_AUDIO_SETTINGS в debug gate manifest. Никакого mic FGS/background claim; foreground stop ends probe. Real AEC/NS/route state плюс far-end/double-talk/clipping/underrun observations. Начать с known16k path; device-rate/custom processing только если измеренный local path блокирует gate.
6. **Evidence-driven screening и длительный gate.** Сначала local codec/crypto/trace tests и protected timestamp traffic, затем коротко20/40/60ms на одном lane (cap12k соответственно50/100/150B; пересчитать overhead/window). Freeze один профиль. Доступные phone↔host DNS/audio проверки выполнить, не объявлять physical pair PASS. Для final pair:45min50 и80 profiles; silence/mute, external loss vs application drops, 100/300/1000ms bursts, bandwidth collapse/restore, admitted8KiB competitor и максимальный разрешённый TEXT-sized frame/backlog, coordinated restart, relative skew±100/500ppm. Competing frames моделируют нагрузку, не доказывают production bulk pause/TEXT durability. Sequence rollover и timestamp rollover тестировать near-boundary явно:43m41s относится к16-bit sequence, timestamp48k — около24h51m; DTX count не обеспечивает sequence wrap. Common-clock acoustic latency, source-active loss accounting, отдельно normal/outage scenarios и listening evidence. Нет новых branches/dependencies без измеренной причины.

## Current Checkpoint
- Closes: R5 source timing correctness and actual two-phone screening; recheck affected R3/R4.
- Smallest next action: publish verified timing fix, then compare supported direct-path native settings and active-only hop/CPU timing to discriminate remaining asymmetric DNS lateness. No carrier algorithm rewrite.
- Expected evidence: unchanged-frame regression, both actual microphone/playback owners, per-direction arrival/deadline/sink/worker timing and active-only DNS counters. Separate real deadline loss from renderer-induced retirement and setup/teardown noise.
- Stop or replan if: a measured cause requires forbidden scheduler/production changes; choose allowed discriminating experiment first, not larger jitter/window or weaker checks.

## Current State
- Resolved: goal7a37e47, codec4bc5df8, SRTP9bb9ecb, protected probe aeae648, Android d4fec90 and readiness/evidence e1b0f4a pushed; R1–R4 verified. Source/gate instructions in crates/core/VOICE_PROBE.md.
- Last relevant evidence: current focused probe26 PASS and fmt PASS, arm64/API26 native and JVM122/debug/release/androidTest builds PASS. Both actual local8s gates PASS: Moto decoded200/Pacman199, active PLC/late/captureDrop/renderDrop/skips/expiry/underruns0. Packaged Pacman NoLACE31998/DeepPLC1907/lookahead104 PASS. Both actual DNS20s instrumentation/cleanup PASS but asymmetric late87/0 and render expiry160/480 samples are not quality acceptance. Previous broad271 PASS is prior-checkpoint evidence, not the final gate for this new source diff.
- Blocker: second-phone blocker removed by explicit user authorization and current adb evidence. Physical latency/service calibration remain unqualified; continue independent source correction and two-phone tests instead of treating them as a blanket development blocker.
- Next: isolate active transport/CPU timing, repeat paired screening before a long selected-profile gate. Do not start M6 or expand carrier/codec scope.

## Material Decisions
- 2026-10-09: пользователь разрешил второй физический Pacman 192.168.1.55:39631; существующий Main на нём сохранять. Fixture-only timing regression и paired diagnostic measurements входят в R5, не новый call API. Предыдущая blanket blocked closure отменена; missing measurement evidence не блокирует доступную диагностику.
- 2026-10-09: последний user override разрешает implementation/commits/push/тесты на ZY22JFJ5LP; отменяет прежний read-only режим, не разрешает Main reset или production rollout.
- 2026-10-09: для минимального M1-V выбираем existing whole-carrier stop/join/fresh generation; shared bridge RST patch и native diagnostic ABI отложены. Полная M6 приёмка не заявляется.
- 2026-10-09: 50/80 useful-throughput profiles и actual DNS measurement boundary не заменяются outer-IP50k shaper; missing physical endpoint не заменяется host PASS.
- 2026-10-09: safe alternative to remote firewall/deployment changes is an owned source-scoped LAN DNS forwarder/private carrier. It proves real C/UDP DNS plumbing, not public recursion or measured narrow capacity. A delayed-response bounded model is explicitly labelled; no hidden QoS claim.

## Checkpoint History
- 2026-10-09: preflight/audit/contract freeze; implementation и tests ещё не выполнялись.
- 2026-10-09: goal7a37e47 pushed; separate bounded LiveEncoder/LiveDecoder, valid tiny DTX, exact PLC and actual WB40 neural activation host tests PASS. Note contract unchanged. Fresh target/live-check avoids stale native CMake cache; no upstream build changes.
- 2026-10-09: live codec4bc5df8 pushed. SRTP2.7.0 native boundary verified host12 and Android arm64/API26; upstream archive unchanged, licenses retained. Separate debug/.gate Android owner and exact-method tests compile, JVM122 PASS; no device run yet.
- 2026-10-09: SRTP9bb9ecb pushed. Integrated protected probe16 host tests and workspace271 PASS; target/live-check isolated. Full Temurin21 with verified archive resolves missing jlink in prior local JRE. Empty owned ADB server needed legacy USB backend to see authorized Moto; no keys/Main data changed. Actual codec and foreground local audio gates PASS; unnecessary render overwrite and synthetic missed-slot loss diagnosed and fixed before DNS screening.
- 2026-10-09: protected probe aeae648 pushed. Latest Android local8s gate PASS: decoded198, PLC0, late0, renderDrop0, underrun0; independent capture/render progress, verified platform rate actuator and complete pause cleanup. R4 verified; no DNS/two-phone/mouth-to-ear claim.
- 2026-10-09: Android d4fec90 pushed. Actual controlled DNS phone↔host/fresh generation/NativeClient joins PASS;20ms expiry and poor40/60 delivery recorded. Query pressure and confounded delayed-response case are evidence, not reasons to inflate queues. Final stricter local gate decoded201/PLC0/late0/underrun0 PASS; gate permits6–3600s but no long physical gate is claimed. Owned UDP/TCP listeners and six phone fixture directories removed; gate mic permission revoked after results. Main/workingsrv/firewall/identities unchanged.
- 2026-10-09: readiness/evidence e1b0f4a pushed. Final closure: 271 Rust tests and 122 JVM tests PASS, complete Android builds PASS, one authorized Moto confirmed by adb devices -l, owned test listeners absent. R1–R4 resolved; R5 explicitly blocked, no optimal profile or live-call acceptance claimed.
- 2026-10-09: both phones authorized, prior blocked closure resumed. Fixed early PLC, timer-vs-receive ordering, whole-frame expiry and fully-elapsed-slot skipping with deterministic regressions (26 focused tests). Unchanged active zero-drop local gate PASS on both; source regression was not waived. First direct-authoritative physical pair works, while Moto late87/PLC87 versus Pacman0 proves remaining asymmetric delivery needs diagnosis. One failed Python observer was bypassed rather than attributed to the phones; no scheduler/jitter/security/check weakening.

## Artifact evidence
- Previous-checkpoint artifact hashes below refer to aeae648/e1b0f4a, not the newly built timing fix. Pinned carrier d7cd5555a88933053551128ff8b3741ae93049a0 unchanged; actual Moto API35/Pacman API36. Private topology addresses/keys/logs remain ignored, never tracked.
- SHA256 pinned CLI:8205e1434c91a1f7d335a6fdda01af97aee347f0b81da627ba64b0e4433713cd.
- SHA256 host probe:38491d597ef1409a21c7ba44691e9d554de2b8cb5db8217bad331bf239c2c76f.
- SHA256 arm64 lib:1524aa26ce68a7a9c3d6c997280b9dcac4f9d92aea29b649c318e81306dcab7e.
- SHA256 latest gate APK:5c7328cb341bb85d0396d04b434c9dca4cfa06dd7c9aa0d8230be027eca2e693.

## Completion
- Resolved outcomes: R1–R4 prior verified implementation, affected timing correction rechecked on both phones; R5 in_progress. This is not DONE for full M1-V acceptance; no second-phone blocker remains.
- Commands and artifacts: clean allowlist `cargo build --offline --locked -p msgd`, then `MSGD_BIN=<isolated target>/debug/msgd cargo test --offline --locked --workspace --features dmsg-core/voice-probe` — 271 PASS, two unchanged explicit native ignores; `cargo fmt --all -- --check` PASS. `ANDROID_NDK_HOME=<r28c> sh android/build-native.sh --voice-probe` PASS. From android, full JDK21 `./gradlew --no-daemon testDebugUnitTest assembleDebug assembleRelease assembleDebugAndroidTest -PgateInstall=true` PASS; JVM122 PASS. Exact .gate codec/local-audio/DNS methods and sanitized screening evidence above; no connected test/Main install. Binary hashes recorded above.
- Constraint and diff-scope check: authored `git diff --check` PASS; unmodified upstream libsrtp archive retains its original whitespace/license. Diff stays in fixture-only change envelope; no Main/deployed backend/schema/identity/carrier-revision/scheduler changes. Only unrelated original PNG remains untracked; secrets/.local/builds/JDK not staged. Each current run releases owned listeners/audio; gate permission/private fixtures remain for the explicitly authorized continuing two-phone session and must be cleaned after its final results.
- Final status: active again after second-phone authorization; previous checks are checkpoint evidence, not current two-phone acceptance.
