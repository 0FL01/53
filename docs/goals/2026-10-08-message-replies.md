# Goal: Reply на TEXT/VOICE сообщения

Status: complete
Source: пользователь: «холдишь сообщение и жмёшь reply», «по аналогии как в Telegram»; выбран ответ текстом и голосовыми; «fresh db допустима, если это упростит путь внедрения»; после read-only аудита: «делай копию плана в цель, итеративно реализовать и коммит пуш, тесты на телефоне».
Last updated: 2026-10-08

## Objective
Hold → Reply для видимых incoming/outgoing TEXT/VOICE, banner с Cancel, ответы текстом/голосовыми с durable E2E reference, shallow цитата текущего оригинала и переход к нему. Проверить host E2E и настоящий Android UI на fresh isolated `.gate`; сохранить Main/backend, закоммитить и запушить intended изменения.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Durable Reply reference для TEXT/VOICE, одинаковая связь у обоих участников.
  - Source: запрос Reply, подтверждённые TEXT/VOICE и исправленный после аудита план.
  - Acceptance: ответы на свои/входящие TEXT/VOICE, включая видимые own queued; reference сохраняется при reopen/Edit/Delete/retry. Цитата текущая, shallow; target edit обновляет preview; missing/hidden/deleted target не раскрывает старое содержимое. Missing inbound original не мешает сохранению/ACK. Raw text/4000 scalar limit, ratchet atomicity и byte-identical retry сохранены.
  - Primary evidence: existing protocol/Core/FFI/Olm tests и live message_actions/voice_notes с encrypted host pair.
  - Status: verified
  - Evidence: clean-env workspace 222 PASS / 2 existing native-gate ignores; live message_actions 2 PASS, voice_notes 5 PASS. Все восемь sender-relative комбинаций, реальные maximum prekey/normal SEND/FETCH, missing/late/current/terminal projection, atomicity и immutable retries проверены. Parent reviewed codec/header, schema guards, scoped JOIN/privacy и writer/duplicate paths.
- R2: Native Android Reply actions/composer/recovery/quote navigation.
  - Source: Telegram-like hold → Reply и исправленный план.
  - Acceptance: настоящий long-press обоих directions/kinds, banner/Cancel без потери normal text, независимый Edit, recreation. TEXT submission/recovery фиксируют raw text + selector; VOICE фиксирует selector при начале записи. Saved потребляет только соответствующий draft; NotSaved/Uncertain сохраняют его. Недоступный target блокирует новый send, но не отменяет доказанный commit. Quote tap на local original вне initial page; prefetch не отмечает историю прочитанной. Copy body/Markdown/Edit/Delete/voice controls сохранены.
  - Primary evidence: existing JVM draft/recovery/paging tests и два exact-method fresh native phone gates.
  - Status: verified
  - Evidence: final JVM113 PASS (14 suites, zero failures/errors/skips). Moto exact composer/navigation method PASS34.235s: real holds in all four direction/kind cases, body-only Copy, draft/Edit/recreation/Cancel, hidden target, deep gap/175-hidden-row quote jump with meaningful native unread suppression and delayed-callback cancellation,200% landscape IME/48dp Cancel. Real mic method PASS8.062s: frozen target/pause/preview/AudioTrack/background/recreation/unavailable original/Discard→Cancel, no auto-send.
- R3: Актуальные сборки и изолированные тесты на доступном телефоне.
  - Source: последний запрос; AGENTS.md/AUTH_GATES.md.
  - Acceptance: clean-env Rust workspace/codegen/arm64/JVM/debug/release/test APK зелёные; два выбранных `.gate` метода PASS. Clipboard/font/orientation восстановлены; Main identity/install/data и backend не изменены.
  - Primary evidence: команды, package/ABI checks, manual exact-method instrumentation.
  - Status: verified
  - Evidence: official host cdylib/codegen and NDKr28c arm64 PASS; final clean Gradle testDebugUnitTest/assembleDebug/assembleRelease/assembleDebugAndroidTest -PgateInstall=true PASS. Both exact fresh native methods PASS on Moto ZY22JFJ5LP/API35; aapt2 confirms.gate/.gate.test and arm64-v8a. Only owned.gate reset/installed; Main versionCode2/uid10420 unchanged, font1.0 restored; clipboard/orientation restored in finally. No backend/Main operations or Android DNS delivery claim.
- R4: Копия исправленного плана, актуальные контракты, scoped commits/push и closure.
  - Source: последний запрос.
  - Acceptance: эта цель хранит plan/current evidence/closure; current contracts/runbook соответствуют source; только intended source/tests/docs/bindings committed/pushed, без private/build artifacts и чужого PNG.
  - Primary evidence: git status/diff/log, push и remote ref.
  - Status: verified
  - Evidence: plan22aedea and feature b2574b2dfdd41d9dec0061e1740564cdf6adaa31 committed/pushed to origin/master; remote ref matches feature. Reviewed46 intended source/tests/docs/generated-binding files, no private/build artifacts or unrelated PNG. Current contracts/runbook and this closure record complete the deliverables.

### Constraints
- Fresh core schema10 напрямую, strict E2E v2, без миграций/legacy decoder/conversion/autowipe. Server6 и outer wire2 не меняются. Main8/backend5 сохраняются; fresh-DB разрешение применяется к owned disposable `.gate`, не Main.
- Frame16384/ciphertext16304/raw TEXT+EDIT1..4000 Unicode scalar values не ослаблять. Reply metadata не входит в text count. Crypto/VOICE inner manifest1/profile1/chunk AAD/codec/transport revision не переписывать.
- References — Noise sender device32 + original MID16, не sender_ed и не local ID по сети. Incoming ref — annotation, не новый authorization mechanism.
- Sealed storage, TX/CAS/dedup/ACK/status/chronology/read cursor/voice lifecycle сохраняются. Kotlin UniFFI bindings только штатным codegen.
- Build/diagnostics clean allowlist env; secrets не в env/argv/logs/Git. Device gates вручную по exact method, только `.gate`; не connected tests на Main.
- Host DirectTCP E2E + local phone UX не являются successful Android DNS Reply Send/VOICE acceptance. Новую DNS-инфраструктуру/production cutover не добавлять.

### Non-goals
- Swipe/partial quote/threads/forward/cross-chat replies, retarget при Edit, persistent quoted-text snapshots, server backfill/search/centered-history API.
- Новые dependencies/services/workers/cache coordinators/gesture frameworks/migration/negotiation. Read-only аудит существующих by-MID/FETCH collision concerns не создаёт соседние задачи.
- Production/Main rollout/reset; fresh local DB не очищает retained v1 server mailbox, будущий cutover отдельный.

## Change Envelope
- `crates/protocol/src/e2e.rs`; existing core store/chat/voice/history/FFI/Olm и прямые Rust tests/examples/fixtures consumers.
- Android facade/unready/fake, generated bindings; TextUiState/TextSend/VoiceUiState/ChatActivity/NativeUi, chat layout/EN+RU resources; existing JVM tests/current fresh fixtures и два Reply phone methods.
- Current `docs/protocol.md`, `crates/core/R18_API.md`, ARCHITECTURE/WORK_PLAN/AGENTS current-version notes, new AUTH_GATES section и эта цель. Исторические goals/results не переписывать.
- Binaries только build/target, private test fixtures вне Git; unrelated `53-opendesign/` PNG и `.local/slipstream` не трогать.

## Copied implementation plan (audited KISS version)
1. **E2E v2 и real fit.** Event получает `reply_to: Option<ReplyRef>`; ReplyRef состоит из Noise sender_device32 и message_id16. Все kinds имеют fixed51 header: прежние поля0..49, presence50; body51 при0, optional48 ref51..98 и body99 при1. Sender_ed остаётся slice18..50, не18..HEADER_LEN. Flags только0/1; EDIT/DELETE требуют0. Body layouts/VOICE inner manifest не меняются; ENVELOPE_MAX16099. Codec malformed/roundtrip/v1 rejection и actual max replied TEXT prekey/normal SEND/FETCH tests до Android integration; static bound16276 оставляет28bytes, не считается runtime proof.
2. **Fresh schema10 и existing writers.** Nullable reply_ref BLOB48 толькоBASE, отдельный от target_mid; noFK/table/index. Reference сохраняется в исходной ratchet/outbox transaction; controls его не меняют. Shared target resolver None no-op, supplied local ID должен быть visible TEXT/VOICE samecontact eitherdirection; invalid → existing MessageUnavailable. TEXT preflight до bootstrap + repeatedTX check. VOICE newMID проверяет target в TX до session bootstrap. Existing MID проверяет same contact/kind/reply identity и возвращает original до audio/availability/session/trust gates; changed selector → MessageUnavailable, старые None/None и hidden retries сохраняются. Identity comparison читает retained BASE metadata, не visible resolver/current contact keys.
3. **Shallow history only.** HistoryMessage.reply: Option<ReplyInfo>, fields target_local_id, state Available/Missing/Hidden/Deleted, target_revision, direction/kind, bounded raw preview160 и voice_duration_ms. ScopedLEFTJOIN по contact+NoiseDevice+MID+BASE через existing index, одна statement snapshot, noN+1/recursion. Known hidden/deleted сохраняют ID/revision, но TEXT/VOICE content не unseal для preview/duration. CustomDebug не содержит preview. Received/ReceivedMsg/AndroidMsg не расширять; summaries unchanged.
4. **FFI/bindings и восстановление попыток.** Existing TEXT send/queue и VOICE queue получают last Option<i64>, Kotlin facade defaultnull, ordinary Rust callers None. Обязательный host cdylib/codegen, не manual bindings. Android VOICE сначала queue; only typed VoiceSessionRequired → once bootstrap + queue, не trust/session precheck перед duplicate return. Direct return и exactMID recovery доказывают contact/direction/kind/MID/reply identity; noReply != Some(Missing). Typed precommit MessageUnavailable и доказанный mismatch → NotSaved, failed proof read → Uncertain. Не проверять preview/availability для commit proof.
5. **VM-only drafts/actions/quotes.** Небольшой ReplyDraft с immutable selector, optional canonical cached target/failure state; refresh same worker, captured draft-instance + lifecycle/singleflight fence; cancel/reselect даже sameID отвергает stale callbacks. Preview только собственного тела target. Normal Reply скрывается при Edit, не очищается. TEXT OutgoingDraft хранит submittedText+submittedReplyId; begin толькоnewwrite, restoreSubmitted для recovery без изменения current draft. Восстановить visible submitted tuple только если normal text empty AND noReply. finishSaved clears only exact current text+selector match. VOICE freezes selector at begin; pending attempt owns recovery, не перезаписывает unrelated current draft/frozen null. Discard retains Reply, Saved consumes matching selector, errors retain. Cancel требует active/noedit/noVoiceBusy/nopendingWrite/noUncertain, но не page/target/trust readiness; unavailable original → block new send, finish voice preview then Discard→Cancel/change, no audio-retarget feature.
6. **Native actions и navigation.** Reply predicate отдельный от own Edit/Delete, для visible incoming/outgoing TEXT/VOICE/ownqueued. TEXT systemselection сохраняет Copy/Open link; VOICE hold достижим на body/label без перехвата waveform/play/download. Banner внутри existing footer, Cancel≥48dp; quote plain nonselectable/noactive links, You/localpeer по target direction. Jump privateActivitystate+unique generation: exact scopedread, stableID, existing refresh/older/gap/hidden bounded turns; no inserted target island/cursor reset. Fence worker callbacks AND viewport posts/continuations, suppress competing ticker/top autopaging/bottomfollow/read marking through settling. Success только когда actual target child intersectsviewport; all3cursors absent после requiredrefresh и no pending → exhausted. Cancel touchscroll/newjump/pause. Queuedread проверяет navigation generation перед mutation; уже committed legitimate read не откатывать. Reply target merge independent от source revision, включая early return: Missing→Known разрешён, Known→staleMissing/terminal→Available/lower targetrevision запрещены; no-op equality сохраняет selection.
7. **Минимальная приёмка/контракты.** Extend existing core/live/JVM tests без new harness: sender-relative TEXT/VOICE на TEXT/VOICE, invalid/crosscontact/tombstone/late target/reopen/retry, actual maxfit; draft/voice identity/restoration и navigation hidden/gap/cancel/exhaustion/stale merges. Update complete v2 freshVOICE fixture header и current schema assertions, including Rust support/store and MainDev source (не запускать Main). Два exact methods `ReplyGatesTest#freshReplyComposerAndQuoteNavigationOnlyInGatePackage` и `#replyVoiceRecordingRetainsTargetOnlyInGatePackage`: actual holds/Copy/recreation/Cancel/quote tap outside initial50/read suppression/unavailable target, один new-banner200%font+landscapeIME scenario; real mic preview/background/recreation/discard/frozen selector. Navigation fixture использует opposite ingestion/server order, иначе monotonic read assertion vacuous. Нет additional locale/font/size Cartesian matrices. Final cleanenv cargo build msgd+workspace, hostcorebuild+DMSG_GEN_BINDINGS1gen, NDKr28+arm64, GradleJVM/debug/release/gatedtest build; manual selected phone methods, scoped commits/push и closure.

## Material Decisions
- 2026-10-08: fresh DB явно разрешена; safe scope — isolated `.gate`, Main/backend untouched. Strict E2Ev2/core10 без миграций; future mailbox cutover отдельный.
- 2026-10-08: четыре general reviewers и focused read-only сверка разрешили VOICE duplicate identity/recovery, target content guard, Cancel/readiness и posted navigation fence. Received DTO expansion и соседний read-path overhaul исключены.
- 2026-10-08: phone evidence exposed read-only ticker swallowing a visible Reply selection; local start/menu now use draft readiness, not read-only paging readiness. Edit/Delete retain their existing guards. Opposite-ingest fixture measures native unread via Core rather than a retained stale Android SQLite reader.

## Current Checkpoint
- Closed: R1–R4; one closure comparison passed against the frozen contract, current evidence and approved change envelope. No substantive follow-up work in this goal.

## Current State
- Resolved: R1–R4 verified; plan22aedea and featureb2574b2 pushed. E2E/schema10/history/writers, generated ABI, native Android actions/banner/drafts/recovery/navigation implemented; current docs/runbook and closure complete.
- Last relevant evidence: host222 + final JVM113 + native arm64/debug/release/test APK PASS; two actual fresh Moto methods PASS. Complete E2Ev2 fixtures and schema10 assertions; no Main install/reset/backend or DNS-delivery acceptance claim.
- Blocker: none.
- Next: none; objective complete. Main/backend/Android DNS rollout is outside this goal, not unfinished implementation work.

## Checkpoint History
- 2026-10-08: audited plan frozen/copy created after explicit implementation authorization; user PNG untouched.
- 2026-10-08: R1 host/runtime checkpoint PASS; merged JVM/build checkpoint PASS. Phone remains independent evidence; no Main/backend changes or Android DNS delivery claim.
- 2026-10-08: R2/R3 physical checkpoint PASS; final current JVM/debug/release/test build PASS. Main metadata unchanged; current contracts/runbook reviewed, R4 Git/closure checkpoint next.
- 2026-10-08: featureb2574b2 pushed, remote ref verified; tracked/index diff empty, unrelated PNG untouched. Closure check resolves R1–R4 with current runtime/build/phone evidence and preserved constraints.

## Completion
- Resolved outcomes: R1–R4 verified.
- Commands and artifacts: clean cargo build msgd/workspace222; hostcore/codegen; NDKr28c arm64; final Gradle JVM113/debug/release/instrumentation; both manual selected ReplyGatesTest methods. Debug APK SHA256 `2c9ebd9be840ceb9f6af9a02a0ded245ba6ef4a4af3d6df6688698316b409c34`; unsigned release `3c21555fd633e4b4a6d1932de6d105d2cc8bad3a34bc8740327e8bc95b51437c`; test `9befcd7567cbc6a9f2190875ac5fa69dc1cc1ba383070c2a87f91fbcfcd6196c`; arm64 lib `fca631ee6c49face089c48a1f6c6620d462973c11001bfec80bcf07339a81538` (build artifacts not tracked).
- Constraint and diff-scope check:46 intended files only; generated binding from canonical codegen. No migrations/autowipe/legacy decoder, new dependencies/services/workers, transport/codec/AAD/frame/ciphertext changes or quote snapshots. Main UID10420/versionCode2, identity/data/install and backend preserved; user PNG remains unrelated/untracked. Host DirectTCP E2E + local phone UX explicitly not successful Android DNS Reply delivery; retained v1 mailbox cutover remains outside scope.
- Final status: complete.
