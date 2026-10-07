# Goal: голосовые сообщения через DNS

Status: active
Source: пользователь 2026-10-07: «записи и отправления голосовых сообщений», узкий DNS-канал, Telegram-подобный UI/анимации, удаление; «Только fresh-схемы»; последний override: фиксированный «профиль 10 kbit/s», копия плана в цель, итеративная реализация/коммиты/тесты на телефоне, без SHA256-фиксации, KISS/YAGNI/PARETO.
Last updated: 2026-10-07

## Objective
Записать, отправить, вручную скачать, прослушать и удалить собственное E2E голосовое через существующий DNS carrier. Один фиксированный speech-профиль, без сравнительного codec-ресерча. Только fresh core9/server6 и отдельный gate-пакет; текущие main/серверные данные сохраняются.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Фиксированный Opus 10 kbit/s, запись/preview/playback и ограниченное кодирование.
  - Source: последний override и утверждённый план codec/audio.
  - Acceptance: mono PCM16/16 kHz, 20 ms, VBR, VOIP/VOICE, MAX_WIDEBAND/AUTO, encoder complexity10; bundled libopus1.6.1/OSCE и decoder complexity7; DTX/FEC/DRED/BWE/QEXT off. До60s и128KiB итогового encrypted object; корректные lookahead/flush/trim, bounded parser/JNI, без PCM/plaintext audio на диске.
  - Primary evidence: codec/container tests, arm64 native build и настоящий recorder/player gate.
  - Status: pending
  - Evidence:
- R2: Durable E2E blob upload/download через fresh server6/core9, resume и независимый control-path.
  - Source: утверждённый план, ARCHITECTURE §7–8, WORK_PLAN M5.
  - Acceptance: VOICE manifest внутри Olm; случайный blob key/unique chunk nonce, AEAD с index/count/length binding; chunks≤8KiB, одна bulk-передача. ACK только после durable receipt; повтор bytes неизменен; resume после reopen; exact-device ACL/revocation, reservation/quota/TTL. TEXT/DELETE не блокируются полной передачей.
  - Primary evidence: live-msgd integration/ACL/resume tests и actual DNS gate, если отдельный endpoint доступен.
  - Status: pending
  - Evidence:
- R3: Telegram-подобные действия и анимации, интеграция voice в историю и удаление.
  - Source: исходный запрос/утверждённый Android-план.
  - Acceptance: hold/release, swipe cancel/up lock, locked pause/resume/preview/send/discard; mic amplitude/timer/waveform/play/pause/seek/download progress. Voice только manual download; original timeline/unread/paging. Default SelfOnly, Everyone только own Accepted/Delivered; queued SelfOnly не отменяет отправку; EDIT толькоTEXT; late callbacks не воскрешают deleted voice. Существующие Copy/drafts/selection/anchors сохранены.
  - Primary evidence: JVM state/action tests и isolated native UI gates.
  - Status: pending
  - Evidence:
- R4: Релевантные проверки и телефонная приёмка без повреждения существующих identity/data.
  - Source: «тесты на телефоне», AGENTS.md/AUTH_GATES.md.
  - Acceptance: Rust workspace/build/codegen, Android JVM/debug/release/arm64 и выбранные .gate instrumentation методы зелёные. Только Moto ZY22JFJ5LP, без main reset/install и без Pacman. DirectTCP/USB не выдавать за recursive DNS.
  - Primary evidence: команды/selected instrumentation results и границы runtime evidence.
  - Status: pending
  - Evidence:
- R5: План в цели, scoped-коммиты, актуальные контракты и closure.
  - Source: последний запрос пользователя.
  - Acceptance: цель хранит текущую проверяемую state, intended feature/doc commits, без unrelated PNG/secrets/generated databases/.local и без push.
  - Primary evidence: git diff/status/log и финальный closure check.
  - Status: in_progress
  - Evidence: цель создана до реализации; исходное tracked дерево чистое.

### Constraints
- Fresh core9/server6 ONLY. Без migrations/plain→sealed/legacy decode/autowipe. Existing production8/5 несовместимы и не обновляются этой задачей.
- Не менять DNS/QUIC/scheduler/carrier ревизию и работающий tunnel/backend. Изолированный gate backend со своими данными/секретами/портом.
- Build/diagnostics в clean allowlist env; секреты только owner-only/read-only файлы, не argv/env/logs/Git.
- Никаких ручных SHA256 registry/source/artifact pins. Не удалять существующие crypto recipient binding, AEAD, TLS/Noise pins или Cargo integrity.
- Сохранить одну canonical core_messages. Ciphertext chunks/queue/cache внутри core.db для existing sealed snapshot.
- UniFFI — команды/DTO/готовая bounded encoded note, неPCM и неoutbox ciphertext. JNI — bounded audio batches. Bindings генерировать, не править вручную.
- Main/Pacman/их аккаунты и сообщения не трогать. Fresh gate на Moto допустим; wipe только gate. Закрытие старой задачи не является разрешением нового main reset.

### Non-goals
- Codec сравнительный benchmark/MOS/WER/corpus study, Lyra/Codec2/TFLite, второй codec/quality selector, gzip/zstd, DTX-v2, BWE, server transcoding, live calls/background audio.
- Photos/files UI, editing audio, migrations, production rollout, новый retry/framework/media stack, отдельная audio service/.so.

## Change Envelope
- Cargo workspace/dependencies, минимальный bundled opus-sys, core voice codec/JNI.
- protocol E2E/blob DTO; server fresh schema/blob handlers/mailbox ACL/backup/GC и tests.
- core store/chat/history/FFI/voice transfer и relevant fixtures/tests.
- Android facade/audio/coordinator/state/views/chat/native UI/resources/manifest/storage и relevant gates; regenerated bindings.
- ARCHITECTURE §8, WORK_PLAN M5, protocol/R18/AUTH/current acceptance docs. Call §9/M6 не менять.
- Локальные private disposable fixtures только ignored .local; бинарники только build/target. Новый публичный DNS endpoint не предполагать существующим и не менять production для его получения.

## Copied implementation plan (latest KISS override applied)
1. Bundled libopus1.6.1 static в существующую libdmsg_core.so: CMake OSCE ON, DRED/fixed-point/shared/programs OFF, hardening ON; модели C/H входят в source bundle, без build/runtime downloads. Без ручного checksum pin.
2. VoiceCodec encoder/decoder с одним state на note:20ms320samples, batching100ms1600samples, sample_count≤960000, get-lookahead/flush/end trim. Compact versioned packet container, canonical bounded ULEB128 lengths. NoLACE activation/normal packet tests; missing packet не скрывать PLC.
3. E2E kindVOICE=4 с versioned fixed-profile manifest; существующие TEXT/EDIT/DELETE vectors неизменны. Blob RESERVE/STATUS/PUT/FINISH/GET/SEND_MEDIA, wire2 framing≤16KiB, immutable ciphertext chunks≤8KiB, AEAD аутентифицирует порядок/длины без дополнительного checksum framework.
4. Fresh server6: durable temp/sync/rename→receipt→ACK, duplicate PUT samebytes only; mailbox+recipient-device ACL+dedup вTX, quotas reserved+complete,24h/7dTTL, coherent backup/GC. Сервер не читает codec/key/audio.
5. Fresh core9: VOICE canonical rows+sealed manifest и subordinate transfer/chunk tables внутриDB; ratchet/event/chunks queue воднойTX; stable attemptMID/exact reconciliation; first voice bootstraps session без обязательного первогоTEXT. Basekind TEXT|VOICE, controls EDIT|DELETE; original order/status. Pending DELETE terminal; EDIT cannot targetVOICE.
6. Один app-scoped bulk worker. Rust opaque transfer prepare/commit — short DB/store-lock; advance(network) outside lock, persistent own Noise stream/runtime/window1/cancel timeout. Очередные TEXT/DELETE проходят независимо; hidden queued uploads продолжаются. Cache bounded/LRU excludingqueued/active, cache clear preservesqueue/snapshot.
7. AudioRecord/AudioTrack dedicated threads и narrow JNI; permission explicit, audiofocus/noisy handling, onPause stops mic into UNSENT preview/player stops. Memory-only preview retainedconfiguration/notprocessdeath; encryptedqueue durable. No background audio/auto-send onpause.
8. Native Light/Square voice controls with Telegram-style gestures/animations: mic/send transition, realRMS pulse/timer, lock/cancel, pause/preview, waveform+seek/manualdownload progress. StableID view invalidations, неwholeadapter каждуюframe; accessibility buttons/48dp/ENRU/zeroanimations. VoiceDelete scopes matchTEXT, noEdit/placeholder/late resurrection.
9. Unit/live/JVM/codegen/NDK/debug/release и Moto .gate actual recorder/UI/reopen/delete/resume checks. ДляDNS fresh6 нужен separate reachableendpoint: если он недоступен без production changes, зафиксировать ровно этот blocker, не подменять gate localhost/USB.
10. Обновить контракты/цель, scoped commits и один closurecheck. Остановиться после DONE.

## Current Checkpoint
- Closes: R1/R2/R5.
- Smallest next action: зафиксировать goal; параллельные независимые codec, protocol/server, core и Android workstreams с согласованным API; затем интеграция и проверки.
- Expected evidence: focused package tests и feature build, без schema8/5 data mutation.
- Stop or replan if: наблюдаемый endpoint/device/access blocker. Продолжать независимые части.

## Current State
- Resolved: контракт/границы зафиксированы; profile10k выбран пользователем без сравнительного gate.
- Last relevant evidence: baseline ba895fe, только unrelated user PNG untracked.
- Blocker: fresh DNS endpoint ещё не подтверждён; проверяется без изменения production.
- Next: feature implementation.

## Material Decisions
- 2026-10-07: последний user override снимает comparative benchmark и ручную SHA256-фиксацию; crypto validation/functional gates остаются.
- 2026-10-07: narrow phone authorization — только fresh .gate Moto; main/Pacman не участвуют.

## Checkpoint History
- 2026-10-07: preflight/goal; tracked baseline clean, plan copied with latest override.

## Completion
- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.
