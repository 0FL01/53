# Goal: настоящий DNS fallback и Wi-Fi/LTE handoff

Status: complete
Source: пользователь 2026-10-06 утвердил приведённый ниже план, копию в цель, итеративную реализацию, commit и deploy.
Last updated: 2026-10-06

## Objective
Android подключается через DNS активной сети, только после неудачи — через базовые IPv4 DNS Яндекса. Смена Network/DNS корректно заменяет транспорт и будит единственный poll worker, сохраняя account/history/queued ciphertext.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Принятый план (копия решения)
1. Rust supervisor: DNS сети и Яндекс (`77.88.8.8:53`, `77.88.8.1:53`) — отдельные последовательные группы native attempts, не общий multipath список. Успех — настоящий Slipstream Ready. После transient bootstrap failure первой группы join/release, затем резервная. После отказа обеих backoff 1/2/4/8/16/32/60 s и повтор с первичной группы. Working path сохраняется до смены сети/потери транспорта. После established loss backoff и повтор с DNS сети. Pin/config terminal errors не становятся fallback. Первичные DNS сохранены в profile; выбор группы runtime-only. Согласовать endpoint wait с двумя bounded группами (до 3 s/resolver, максимум 8+2).
2. Kotlin: единый runtime snapshot `Network + первичные DNS` для foreground и FGS. Другой Network даже с теми же DNS заменяет транспорт; тот же snapshot не сбрасывает соединение; late old-network events не отменяют новую сеть. Использовать существующий dnsNetworkChanged без передачи network handle native.
3. Worker: заменить interval sleep на пробуждаемое ожидание; sticky wake для события перед wait, coalesce повторов, единственный worker. После применения сети reconnect/fetch/retry без ожидания 15 s/5 min. User Stop приоритетен, late poll запрещён; interrupt остаётся stop-механизмом.
4. Rust local endpoints/counts и JVM tests; workspace/ARM64/debug/release; physical `.gate` actual Yandex fallback и Wi-Fi↔LTE, FGS off/economy wake, account/queued ID+ciphertext/dedup. DNS packet counts/TX/RX bytes за attempt и failed cycle измеряются, не выводятся из timeout. Fixture evidence отделяется от actual recursive Android acceptance.
5. Client deploy без reset identity; evidence/docs и intended commit. Backend/wire/schema не меняются; server restart/rollout не нужен ради клиентской политики.

## Frozen Contract
### Required Outcomes
- R1: последовательный fallback и bounded retry
  - Source: принятый план §1.
  - Acceptance: primary Ready → backup traffic0; transient primary failure → backup Ready; both fail → backoff/primary; working backup retained; stop/pin/config fail-closed; one native instance.
  - Primary evidence: targeted Rust tests с controlled native endpoints/counters.
  - Status: verified
  - Evidence: `cargo test -p dmsg-core --lib dns::tests` 5/5; explicit pinned C native gate 1/1 (primary Ready/backup0, fallback Ready retained across same-profile calls, both failed→backoff1→primary, cancel<1s, invalid pin terminal/backup0). `slipstream-sys --test loopback -- --ignored` passed full pin/streams/loss. Workspace green.
- R2: Network+DNS handoff и wake
  - Source: принятый план §2–3.
  - Acceptance: same-DNS/new-Network restart, duplicate/late event no extra cancel, foreground off-FGS detection, sticky wake without worker overlap/late stopped poll.
  - Primary evidence: JVM state/worker tests и physical network transition.
  - Status: verified
  - Evidence: shared per-DB Network+DNS runtime, cancellation before store lock, coalesced latest apply and sticky wake. 43 JVM tests green (5 network/4 worker tests): same-DNS/new-Network, duplicate/late events, latest apply, one-worker/Stop. Moto USB physical foreground Wi-Fi→LTE→Wi-Fi and economy Wi-Fi→LTE→Wi-Fi passed; economy successful new poll2713ms/4932ms after default-network availability, bypassing300s. Saved-key resume/account preserved. Actual networks had different DNS; same-DNS case proven by JVM state test, not claimed as physical. Earlier non-radio wake4729ms/no late poll after Stop remains valid.
- R3: реальный DNS путь, сохранность доставки и измерение трафика
  - Source: принятый план §4 и запрос «какие попытки … какой объём трафика».
  - Acceptance: actual Yandex DNS/QUIC ready/auth; physical Wi-Fi/LTE recovery без password; same queued mid/ciphertext, receive1 then0/Delivered; reported packet/TX/RX measurement with boundary and scenario.
  - Primary evidence: explicit isolated `.gate` methods + bounded DNS traffic counters/capture.
  - Status: verified
  - Evidence: actual Yandex fallback/Noise key resume on Moto API35 + A142P API36 with primary loopback UDP sink: 10 packets/2740 DNS payload TX bytes, primary profile preserved across repeat commands. Actual Yandex retry retains queued mid/ciphertext→Accepted, second phone receive1 then0/skips0, persistent Delivered reopen. Final USB physical handoff gate passed33.4s, foreground off-FGS and economy both directions; identical queued mid/ciphertext/account retained through LTE submission and repeated network wake/retry. Then native recursive-DNS peer receive1 then0/all skips0/exact plaintext exactly once in history; phone Delivered persisted across process/activity reopen, outbox removed, one history row. Local failed-cycle/phone UID traffic measurements below are separate scenarios, not LTE byte/battery measurement.
- R4: сборки, deploy, docs и commit
  - Source: «делай копию плана в цель и итеративно реализовать и коммит, деплой».
  - Acceptance: relevant Rust/JVM/ARM64/APK gates green; updated client installed/launched without reset; evidence recorded and intended commit created.
  - Primary evidence: commands/results, installer readiness, git status/log.
  - Status: verified
  - Evidence: implementation commit `293a723`. Rust build/workspace/codegen, NDK r28c ARM64, JVM43/debug/release/gated test APK/main export green. Current `53.apk` installed on Moto main without reset; installed APK and ARM64 native bytes match build, UID/first-install preserved; Dialogs startup; schema6, wrapped key/device/account/history2/contacts byte-identical before/after install AND real main UI DNS key-resume check. Main on second phone remained absent/unchanged.

### Constraints / non-goals
- Не менять VPN policy/physical network selection/socket binding, C DNS/QUIC/scheduler, backend/deploy topology, wire/schema/crypto. Current active Android network остаётся источником; network identifier только runtime comparison.
- Только указанные два IPv4 fallback addresses. Длительный Doze/screen-off R9 и restricted-egress матрица остаются deferred/outside scope.
- Сохранить full-cert/Noise/Olm/Keystore/account/history/outbox invariants; не reset main/не менять server pins/исходный tunnel. Tests destructive только `.gate`, secret fixtures file-only; clean allowlist tools без credential env/dumps.
- Без новых services/workers/dependencies/store/migrations; Kotlin bindings не редактировать вручную.

## Change Envelope
- `crates/core/src/dns.rs`, directly affected core tests/FFI only if needed; existing native boundary narrowly for traffic measurement only if controlled endpoints cannot provide it.
- `android/.../DnsNetwork.kt`, `UniFfiFacade.kt`, `DmsgService.kt`, `SingleWorker.kt`, direct tests/device gates.
- `docs/goals/`, `android/AUTH_GATES.md`, APK build/install artifacts gitignored; main install update retains identity.
- Live devices: app-scoped fixture/ordinary Wi-Fi/LTE toggle with independent control connection; backend scoped disposable signup-invitation issuance/cleanup, no backend rollout/reset required.

## Current Checkpoint
- Closes: remaining physical-transition portion of R2/R3, verified; closure complete.
- Next: none. Do not expand this completed goal to VPN, long Doze or restricted egress.
- Evidence: owner-only `.local/dns-handoff/usb/lte/handoff-proof.json`/instrumentation log, native fetch/history and phone Delivered logs, main artifact/data/metadata proof.

## Current State
- Resolved: R1–R4. Blocker: none.
- Last relevant evidence: final `.local/dns-handoff/usb/lte/` USB instrumentation and native peer logs/proofs. Selected-SIM data enabled after explicit user permission (`svc data enable`); effective `mIsDataEnabled=true`, LTE registration. Physical foreground/economy handoff and subsequent exact-once delivery all passed; no ADB reverse/SSH message bridge.
- Deployment: installed main APK matches root `53.apk`10097587 bytes and current ARM64 native build. Raw encrypted identity/account/history/contacts, wrapped key and install metadata unchanged before/after tests, cleanup AND final actual main UI DNS saved-key check. Main cold startup577ms; no reset/reinstall needed in this follow-up.
- Cleanup: gate/test packages, native peer DB/keys/credentials and only this run's consumed invite files removed. Raw telephony dumps removed; sanitized prerequisite proof retained. Server test accounts intentionally not wiped. Wi-Fi enabled/restored, selected-SIM mobile data remains enabled as user authorized; second-SIM/APN/roaming/VPN unchanged. Economy preference restored, gate FGS stopped, main launched.
- Earlier rejected approaches: initial Wi-Fi-only control loss was not PASS; later USB45s prerequisite timeout was caused by per-SIM data disabled despite global `mobile_data=1`. User permission changed that condition; final experiment passed without relaxing tests or contract.

## Traffic evidence (one run, not a bandwidth guarantee)
| Scenario | DNS TX packets / payload bytes | DNS RX packets / payload bytes |
|---|---:|---:|
| Local C carrier primary Ready + 250ms | 14 / 3602 | 14 / 6755 |
| Unused backup when primary Ready | 0 / 0 | 0 / 0 |
| Local silent resolver attempt (~3s) | 10 / 2720 | 0 / 0 |
| Local failed cycle (1 primary + 1 backup) | 20 / 5440 | 0 / 0 |
| Moto/A142P silent primary before actual Yandex fallback | 10 / 2740 | 0 / 0 |

Local values exclude UDP/IP/link overhead and application Noise/messages. They use a controlled DNS relay/sink and the pinned real C server, not a mocked Ready. The production-domain payload differs from the synthetic suffix; never multiply these bytes into a universal quota.

Android UID deltas for actual fallback + reconnect/refill/fetch/retry/reconnect: Moto TX179052/RX110724 bytes in12787ms; A142P TX193290/RX123747 in15618ms. These are **aggregate UID counters**, including primary loopback, both backup paths, Noise/account operations and prekey refill, not isolated QUIC handshake bytes or cellular billing. Neither UDP payload counters nor UID totals prove battery impact.

## Material Decisions
- 2026-10-06: latest scope excludes VPN binding/selection; only fallback+handoff+wake, measurement, tests and client deploy.
- 2026-10-06: compatible account/core schema6 retained; server deploy is unnecessary for client-only policy.
- 2026-10-06: user objected to switching Wi-Fi under Wi-Fi ADB; all later device work leaves radios untouched. Radio gate is USB-guarded; unchanged handoff finish line remains blocked rather than waived.
- 2026-10-06: user supplied independent USB and removed second phone. Resume unchanged R2/R3 using USB Moto plus disposable native peer over actual recursive DNS; no main reset.
- 2026-10-06: user explicitly authorized «включай LTE/Wifi, доступ есть». Enabling selected-SIM data and ordinary Wi-Fi toggles now allowed; APN/roaming/VPN/main identity remain unchanged.

## Checkpoint History
- 2026-10-06: approved plan frozen before implementation.
- 2026-10-06: R1 + Kotlin implementation completed; isolated native gates/traffic and JVM43 green. System Perl lacked FindBin; documented `OPENSSL_SRC_PERL=$HOME/miniconda3/envs/jnabuild/bin/perl` built ARM64 successfully (no dependency/source workaround).
- 2026-10-06: actual Yandex passed on both phones; real queued Yandex retry→peer receive1 then0→Delivered passed. First .gate was empty after earlier cleanup, fresh disposable accounts provisioned. Initial peer UI gate failed with screen/focus prerequisite (transport receive1/0 already passed); final fresh message/UI sequence green after waking screen. An Accepted gate ran after the fallback probe had fetched the peer mailbox and correctly reported Delivered; reordered fresh message sequence passed, expectations unchanged.
- 2026-10-06: R4 main compatible install preserved schema/key/identity/account/history/contacts; live UI DNS check passed. Server remained healthy/schema5/invite_only, no topology/schema/pin/tunnel changes.
- 2026-10-06: `293a723` committed intended13 files, no `.local`/binaries/credentials/user PNG. Closure: R1/R4 verified; R2/R3 blocked solely for physical radio acceptance. Both disposable gate packages and their local credentials removed; only this run's consumed remote invitation files removed. Server accounts intentionally not wiped, main device identity/history retained.
- 2026-10-06: user provided USB Moto, second phone removed. Rebuilt gated APK and isolated-target native peer from current core, fresh file-only signup/session passed. Guarded radio experiment kept USB and restored Wi-Fi but cellular never became default: effective data disabled for both SIMs. No APN/data/roaming changes made. Same queued ciphertext completed via actual Yandex→native DNS peer receive1 then0/exactly once history→phone Delivered. Main data/metadata equality, cleanup and main startup passed. R2/R3 remain blocked for cellular prerequisite, not code failure.
- 2026-10-06: user explicitly authorized LTE/Wi-Fi enablement. Selected-SIM data enabled; effective state true. Fresh isolated queue/session + USB handoff passed33.4s; foreground both directions, economy poll2713ms/4932ms, account/mid/ciphertext unchanged. Native peer receive1 then0/exactly once history and phone Delivered reopen passed. Installed main APK/native match build, same encrypted identity/history/Keystore wrapping bytes/metadata after tests, cleanup AND main real DNS key resume. Wi-Fi restored; selected data stays enabled, APN/roaming/VPN/second-SIM unchanged. Closure R1–R4 verified; complete, no next checkpoint.

## Completion
- Resolved outcomes: R1–R4 verified; no waiver or reduced acceptance.
- Commands and artifacts: clean `cargo build -p msgd -p dmsg-core`, `cargo test --workspace` (168 passed,2 explicit native fixtures ignored in default run then both executed successfully), `DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings`, `cargo fmt --all -- --check`; documented Perl + NDK r28c `android/build-native.sh`; JDK21 clean `testDebugUnitTest assembleDebug assembleRelease assembleDebugAndroidTest -PgateInstall=true`, main `export53Apk`; `dev-install.py --serial <Moto Wi-Fi ADB> --no-build` WITHOUT reset; actual main UI DNS check and private before/after/hash proof.
- Final physical commands: clean `python3 .local/dns-handoff/usb/run.py queue`, `handoff`, `delivery` (explicit individual instrumentation methods, zero skips; native peer actual DNS); `artifact`, `snapshot after`, `cleanup`, `main-probe`, final `snapshot after` passed. The private harness is disposable/evidence orchestration, not tracked application code. Final phone cold startup577ms.
- Artifact: root gitignored `53.apk`, 10097587 bytes, exact installed bytes and native bytes matched, including final USB check. Main remained at Dialogs; second main untouched. No credentials/binaries in commits.
- Constraint and diff-scope check: schema/wire/pins/crypto/C transport/VPN/topology/original tunnel unchanged. User's untracked design PNG preserved. No added dependencies/services/workers/store/API. Native tests/phone probes used scoped file fixtures; original Stop/account/ratchet/dedup guarantees covered.
- Final status: **DONE**. Closure compared all frozen outcomes and affected constraints to current successful evidence. Physical Wi-Fi/LTE prerequisite unlocked by explicit user permission and acceptance passed; no known blocker or code failure. No substantive follow-on work under this goal.
