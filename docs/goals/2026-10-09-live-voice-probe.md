# Goal: M1-V — защищённый live voice probe через узкий DNS tunnel

Status: active
Source: пользователь 2026-10-09: аудит media-плана, затем «делай копию плана в цель и итеративно реализовать и коммит пуш, так же телефон для тестов использовать»; разрешён Moto ZY22JFJ5LP. ARCHITECTURE §7–9; WORK_PLAN M1-V, не M6.
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
  - Status: in_progress
  - Evidence: frozen goal7a37e47 pushed to origin/master. Clean-env SSH push потребовал штатный SSH_AUTH_SOCK (без него publickey denied); для build/test agent socket не передаётся. Пользовательский untracked PNG не трогать/не stage.
- R2: Отдельный bounded live Opus API, без изменения voice-note контракта.
  - Source: аудированный media-план, ARCHITECTURE §9, переиспользование голосовых сообщений из исходного запроса.
  - Acceptance: исходный кандидат mono PCM16/16kHz, VOIP, 12 kbit/s, 40ms/640samples, VBR с реальным max_data_bytes=100, encoder10/decoder7, WB maximum/AUTO, DTX/PLC. FEC/DRED/BWE/QEXT off. Legal tiny DTX packets допустимы, zero-byte incoming packet запрещён; duration/mono/bandwidth/cap проверяются. Existing 10k/20ms DVN API/validation неизменны. NoLACE и Deep PLC проверяются по actual decoder path, не только CTL readback.
  - Primary evidence: live codec tests, existing voice_codec/voice-note regressions, host/Android native linkage.
  - Status: in_progress
  - Evidence: live API host6 tests + six note-container regressions PASS; separate core voice_codec6 PASS. Actual WB40 NoLACE and isolated Deep PLC4/5 change decoded PCM. Fresh target/live-check test/clippy(-D warnings)/fmt PASS; старый clippy CMake cache attempted /usr/local install, fresh target resolves without privilege/config change. Android packaged activation remains for R4 checkpoint.
- R3: Memory-only protected duplex media/feedback/relay с ограниченными очередями и retirement старой generation.
  - Source: аудированный media-план; ARCHITECTURE §7–9.
  - Acceptance: один bidirectional media lane и один control lane на endpoint, независимые Noise states; RTP/SRTP AES_CM_128_HMAC_SHA1_80, SRTCP feedback. Один pending plaintext media packet + один committed frame; deadline drop до Noise, после commitment — finish ordered frame либо уничтожить lane. Remote-authenticated terminal progress ограничивает admitted bytes; local writes не считаются ACK. Relay не получает media keys и не декодирует звук. Fresh generation/Noise/SRTP keys/SSRC/codec/queues после coordinated fixture restart; старый material не replay.
  - Primary evidence: protected duplex/crypto/parser/queue/credit/generation tests и two-endpoint probe results.
  - Status: pending
  - Evidence:
- R4: Реальный foreground Android audio probe на разрешённом телефоне.
  - Source: последний запрос, аудированный Android atom; AGENTS.md/AUTH_GATES.md.
  - Acceptance: visible target-UID host только .gate, explicit mic permission, real AudioRecord/AudioTrack, bounded 10ms JNI batches и independent capture/render/media progress; VOICE_COMMUNICATION/focus/effects/route observations. Playback queue учтена в deadlines, clock correction измеряется, lifecycle cleanup/cancel не зависит от DB/Olm. Main/Pacman/их identity/data не изменяются.
  - Primary evidence: selected exact-method .gate instrumentation на ZY22JFJ5LP, arm64/JVM/debug/release builds и real mic/playback/cleanup evidence.
  - Status: pending
  - Evidence: adb devices -l подтверждает только ZY22JFJ5LP; второй физический endpoint пока отсутствует.
- R5: Честный сквозной DNS/voice experiment и решение по профилю.
  - Source: исходный запрос/исследование, ARCHITECTURE §9.173–177, WORK_PLAN M1-V.
  - Acceptance: timestamp screening и duplex audio по actual DNS, полезная граница byte accounting явно указана; controlled 50/80 application service model отделена от observed DNS service rate. Для полной physical приёмки — два реальных Android, оба upload активны, ≥30min (план 45min) выбранного профиля; common-clock mouth-to-ear в обоих направлениях, late active duration включает pre-send drops, stalls/queue drift/bytes/query rate/ресурсы/понятность. Не заменять physical pair одним phone+host, USB/DirectTCP либо RTT/2. Нормальная цель p95≤400ms/late≤2%; 700ms/5% только кандидат экспериментального пилота, не low-latency обещание. Негативный результат сохраняется как результат, не маскируется изменением gate.
  - Primary evidence: sanitized run summaries, exact topology/config/build versions, source/sink timing и профильное решение с границами evidence.
  - Status: pending
  - Evidence: независимая phone↔host часть доступна; двух физических телефонов текущий adb preflight не предоставляет.

### Constraints
- Закреплённый vendor/slipstream d7cd5555a88933053551128ff8b3741ae93049a0; не править DNS/QUIC/congestion/scheduler, vendor/.local исследовательский checkout или working deployment.
- Один native carrier на endpoint; fixture probe отказывает при занятом runtime, не останавливает чужой owner. Никаких обязательных HTTPS/STUN/TURN/FCM или server transcoding.
- Секреты только owner-only/read-only private files, не Git/argv/env/logs. Relay получает лишь public routing/trust descriptor, не SRTP keys. Production audio не сохраняется; запись разрешённого test material — отдельная измерительная fixture.
- Fresh server7/core10/wire2/E2Ev2 и Main8/backend5 остаются прежними: никаких DB/schema/auth/message-format changes, migration/autowipe или production rollout.
- Build/diagnostics — clean allowlist environment. External disposable harness имеет отдельный CARGO_TARGET_DIR, не перетирает host codegen artifacts.
- Только ZY22JFJ5LP, .gate/.gate.test и exact-method am instrument. Gate install разрешён, Main reset/install — нет. Не stage исходный unrelated design PNG.

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
- Closes: R2 host part, затем R3.
- Smallest next action: commit/push live codec checkpoint; integrate verified SRTP boundary and implement private duplex probe/relay.
- Expected evidence: bounded live codec/unchanged notes; crypto and duplex/admission/generation tests; Android activation later in R4.
- Stop or replan if: изменение требует production rollout/schema/carrier rewrite либо иной physical device permission.

## Current State
- Resolved: frozen goal pushed; отдельный live codec host checkpoint green. Independent SRTP native boundary implemented and reviewed, not yet committed: host12 tests/fmt/clippy and arm64/API26 build PASS in isolated target/srtp-check.
- Last relevant evidence: live test6+note regression6/core voice_codec6, isolated clippy/fmt PASS; adb devices -l показывает единственный разрешённый Moto.
- Blocker: для R5 physical two-phone acceptance нужен второй явно разрешённый физический Android; это не блокирует R2–R4 и phone↔host evidence.
- Next: live codec commit/push, protected duplex probe checkpoint.

## Material Decisions
- 2026-10-09: последний user override разрешает implementation/commits/push/тесты на ZY22JFJ5LP; отменяет прежний read-only режим, не разрешает Main reset или production rollout.
- 2026-10-09: для минимального M1-V выбираем existing whole-carrier stop/join/fresh generation; shared bridge RST patch и native diagnostic ABI отложены. Полная M6 приёмка не заявляется.
- 2026-10-09: 50/80 useful-throughput profiles и actual DNS measurement boundary не заменяются outer-IP50k shaper; missing physical endpoint не заменяется host PASS.

## Checkpoint History
- 2026-10-09: preflight/audit/contract freeze; implementation и tests ещё не выполнялись.
- 2026-10-09: goal7a37e47 pushed; separate bounded LiveEncoder/LiveDecoder, valid tiny DTX, exact PLC and actual WB40 neural activation host tests PASS. Note contract unchanged. Fresh target/live-check avoids stale native CMake cache; no upstream build changes.

## Completion
- Resolved outcomes: pending.
- Commands and artifacts: only read-only Git/adb preflight so far.
- Constraint and diff-scope check: no implementation/runtime changes yet; unrelated PNG retained.
- Final status: active.
