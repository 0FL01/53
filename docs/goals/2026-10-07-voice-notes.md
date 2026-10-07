# Goal: голосовые сообщения через DNS

Status: complete
Source: пользователь 2026-10-07: «записи и отправления голосовых сообщений», узкий DNS-канал, Telegram-подобный UI/анимации, удаление; «Только fresh-схемы»; последний override: фиксированный «профиль 10 kbit/s», копия плана в цель, итеративная реализация/коммиты/тесты на телефоне, без SHA256-фиксации, KISS/YAGNI/PARETO.
Last updated: 2026-10-08

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
  - Status: verified
  - Evidence: Opus controls + six core codec tests PASS, explicit host codegen and NDKr28c arm64 PASS; Moto native partial/trim/seek and real microphone/preview/AudioTrack/lifecycle PASS.
- R2: Durable E2E blob upload/download через fresh server6/core9, resume и независимый control-path.
  - Source: утверждённый план, ARCHITECTURE §7–8, WORK_PLAN M5.
  - Acceptance: VOICE manifest внутри Olm; случайный blob key/unique chunk nonce, AEAD с index/count/length binding; chunks≤8KiB, одна bulk-передача. ACK только после durable receipt; повтор bytes неизменен; resume после reopen; exact-device ACL/revocation, reservation/quota/TTL. TEXT/DELETE не блокируются полной передачей.
  - Primary evidence: live-msgd integration/ACL/resume tests и actual DNS gate, если отдельный endpoint доступен.
  - Status: verified
  - Evidence: core live voice four tests + server live blob two tests PASS. Moto actual native UDP DNS/Noise/Olm: own fresh peer receives/decodes two real-mic notes; reverse 12s/192000samples blob15877bytes uploads, first8192bytes durable download then new-process UI resumes. Control status/tombstone/original chronology and restart-window manifest retry verified. Controlled stub resolver, not default ISP delegation.
- R3: Telegram-подобные действия и анимации, интеграция voice в историю и удаление.
  - Source: исходный запрос/утверждённый Android-план.
  - Acceptance: hold/release, swipe cancel/up lock, locked pause/resume/preview/send/discard; mic amplitude/timer/waveform/play/pause/seek/download progress. Voice только manual download; original timeline/unread/paging. Default SelfOnly, Everyone только own Accepted/Delivered; queued SelfOnly не отменяет отправку; EDIT толькоTEXT; late callbacks не воскрешают deleted voice. Существующие Copy/drafts/selection/anchors сохранены.
  - Primary evidence: JVM state/action tests и isolated native UI gates.
  - Status: verified
  - Evidence: JVM85/zero skips; four local VoiceGates + encrypted snapshot gate PASS; eight DNS sequence methods plus interrupted-own-upload recovery PASS. Real hold/release, lock/preview/send, manual download/play/seek/recreation, default SelfOnly/cancel/peer unchanged, Everyone while playing→peer terminal tombstone/base+control Delivered. Content-aware waveform refresh preserves system Copy selection.
- R4: Релевантные проверки и телефонная приёмка без повреждения существующих identity/data.
  - Source: «тесты на телефоне», AGENTS.md/AUTH_GATES.md.
  - Acceptance: Rust workspace/build/codegen, Android JVM/debug/release/arm64 и выбранные .gate instrumentation методы зелёные. Только Moto ZY22JFJ5LP, без main reset/install и без Pacman. DirectTCP/USB не выдавать за recursive DNS.
  - Primary evidence: команды/selected instrumentation results и границы runtime evidence.
  - Status: verified
  - Evidence: parent clean-env cargo build msgd/workspace PASS(core64+one existing ignore,protocol55,all live/auth/action/blob suites); explicit host bindings/NDKr28c arm64; final JVM85/debug/release/scoped lint PASS. Release8783352bytes/zipalign16KiB PASS. Moto14 selected methods/zero skips; no Main/Pacman install/reset/access. Two pre-existing native-host ignores unchanged.
- R5: План в цели, scoped-коммиты, актуальные контракты и closure.
  - Source: последний запрос пользователя.
  - Acceptance: цель хранит текущую проверяемую state, intended feature/doc commits, без unrelated PNG/secrets/generated databases/.local и без push.
  - Primary evidence: git diff/status/log и финальный closure check.
  - Status: verified
  - Evidence: plan8a71152, atomic featureefc3b70 and this contract/closure commit. Intended diff/status/log reviewed; original upstream kept, owned-code formatting PASS. No user PNG/secrets/.local/build databases staged, no push. Owned3containers/image/firewall rule/remote data and2gatepackages/30local private fixtures removed; working backend healthy/pong.

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
- Closes: R1–R5, complete.
- Smallest next action: none; stop. Production rollout requires separate explicit authorization.
- Expected evidence: all outcomes verified, feature committed, scoped cleanup and contract closure recorded.
- Stop or replan if: new user scope; no additional audits or hardening.

## Current State
- Resolved: R1–R5; fixed10k codec/protocol/server6/core9/Android; plan8a71152, featureefc3b70. Final Rust/JVM85/native/build/lint green and14 selected Moto methods PASS/zero skips.
- Last relevant evidence: FINISH alone left manifestQueued; minimal existing-control wake after upload/start fixed and new-process recovery PASS. Test permission-revoke runner kill moved to operator after result; native newest-first vs UI oldest-first asserted separately; durable DELETE/peer state replaces timing-dependent Queued sampling. All final phone scenarios repeated on a fresh owned pair.
- Blocker: none. Separate controlled UDP stub resolver/carrier provided real DNS evidence without changing working UDP53/NS/backend; no default-ISP delegation claim.
- Next: none. Owned runtime/secret fixtures cleaned; working backend healthy/pong, Main/Pacman untouched. No production rollout.

## Material Decisions
- 2026-10-07: последний user override снимает comparative benchmark и ручную SHA256-фиксацию; crypto validation/functional gates остаются.
- 2026-10-07: narrow phone authorization — только fresh .gate Moto; main/Pacman не участвуют.
- 2026-10-08: no spare public delegation; isolated private-zone stub resolver/high UDP port allowed only gate client sources. Public deployment details stay ignored; exact temporary firewall rule removed on cleanup.

## Checkpoint History
- 2026-10-07: preflight/goal; tracked baseline clean, plan copied with latest override.
- 2026-10-07: codec/core/server/Android checkpoint; host/live and five local native phone gates PASS. Docker msgd workspace copy includes new opus-sys member. Next isolated DNS proof; production untouched.
- 2026-10-08: fixed missing post-FINISH control wake; interrupted persisted note resumes toAccepted through new UI process. Fresh pair full eight-method DNS sequence and five local methods PASS; reverse multichunk download resumes8192→15877bytes and playback192000samples. Parent broad gates PASS; next scoped commits/cleanup.
- 2026-10-08: featureefc3b70 committed. Owned3containers/image/temporary firewall rule/remote data removed;2gatepackages/30private local fixture files removed. Working backend ping=pong/healthy, other containers retained. Current contracts updated and closure verified; stop.

## Completion
- Resolved outcomes: R1–R5 verified.
- Commands and artifacts: clean-env `cargo build -p msgd --offline && cargo test --workspace --offline` PASS (211passed,2pre-existing explicit ignores); `cargo fmt --all --check`; explicit host `DMSG_GEN_BINDINGS=1` generation; NDKr28c arm64; final JDK17 `testDebugUnitTest assembleDebug assembleRelease lintDebug --init-script localization-lint.init.gradle` and gated-test APK PASS. JVM85 zero failures/errors/skips; release8783352bytes/zipalign16KiB. Fourteen exact Moto native/UDP-DNS methods PASS/zero skips. Sanitized public proofs remain ignored; no audio/secret artifacts committed.
- Constraint and diff-scope check: only intended Cargo/Opus/core/protocol/server/Android/deploy workspace-copy/docs, existing transport revision unchanged. Core9/server6 fresh-only; working server5/main8/Main/Pacman untouched. No comparative benchmark/manual checksum registry/push. Vendored upstream autotools whitespace retained verbatim; owned-code diff check green. Own disposable runtime/credentials/data/packages cleaned, production healthy/pong.
- Final status: complete; no known blocker, no further substantive work.
