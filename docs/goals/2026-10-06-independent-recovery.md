# Goal: независимое восстановление carrier/msgd после crash

Status: complete
Source: пользователь 2026-10-06 утвердил KISS/Pareto-план, итеративную реализацию, production deploy и commit; разрешил scoped loss старых данных.
Last updated: 2026-10-06

## Objective
Отдельное dmsg53 автоматически восстанавливает DNS-доставку после падения carrier
или msgd без каскадного restart/watchdog. Известный crash EDNS OPT устранён.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Принятый план (копия решения)
1. Удалить `network_mode: service:slipstream`: независимые namespaces в существующей
   выделенной bridge-сети. Сохранить static IP carrier/public DNS endpoint. msgd
   получает static IP вне dynamic pool. Одна `DMSG_MSGD_IPV4` задаёт container IP,
   listen и target: private IP:7000, без published TCP. Оставить `unless-stopped`,
   первоначальный порядок msgd → carrier. Не использовать cached hostname
   `msgd:7000`: pinned CLI resolves target один раз при старте.
2. В SPCDNS проверять полный option header и data length до чтения/продвижения
   указателя. Принимать valid four-byte zero-data option; malformed → format
   error, не abort. Один shared overlay server/embedded из той же pinned ревизии.
3. Regression через полный `dns_decode`: empty OPT, zero-data/обычные options,
   headers/хвосты длиной1..3, data overrun, несколько options. Baseline assertion
   воспроизвести, fixed ASan+UBSan/asserts PASS.
4. Backup/scoped production rollout. Независимо убить carrier/msgd и пересоздать
   только msgd: restart policy восстанавливает процесс, peer не перезапускается.
   Два physical Android через actual recursive DNS сохраняют queued ciphertext,
   received1 then0, Delivered/skips0. R9 deferred. Evidence/docs и commit после.

## Frozen Contract
### Required Outcomes
- R1: независимые namespaces/stable закрытый backend
  - Source: принятый план §1.
  - Acceptance: static msgd вне dynamic pool; одна IP-переменная; нет namespace sharing/published TCP7000.
  - Primary evidence: Compose validation и runtime narrow inspect/netns/connectivity.
  - Status: verified
  - Evidence: `docker compose config` resolved MSGD_LISTEN=172.18.0.3:7000, target-address=172.18.0.3:7000, ipv4_address .2/.3, depends_on service_healthy, no published 7000. Runtime: netns carrier `4026532331` ≠ msgd `4026532283`; nft DNAT unchanged to 172.18.0.2:5353; msgd healthcheck healthy/schema5; TCP7000 не опубликован.
- R2: shared EDNS fix и regression
  - Source: принятый план §2–3.
  - Acceptance: valid zero-data принят, short headers/data overruns отклонены без crash; baseline reproduced/fixed sanitizer green; оба build paths используют patch.
  - Primary evidence: targeted native runner, carrier/workspace/native builds.
  - Status: verified
  - Evidence: baseline pinned SPCDNS assertion `len > 4` reproduced (rc −6); fixed build ASan+UBSan PASS 12/12 dns_decode cases; server Dockerfile и slipstream-sys stage.py применяют один `spcdns-opt.patch`; `cargo build -p dmsg-core` rebuild со stamp и `cargo test --workspace` 18 ok/0 failed; production carrier image `32197dab84e2` построен с patch.
- R3: production independent recovery/DNS delivery
  - Source: принятый план §4.
  - Acceptance: carrier/msgd crash auto-recovery без restart peer; msgd-only recreate; оба Android byte-identical retry/received1 then0/Delivered/skips0.
  - Primary evidence: container identity/start/restart proof + explicit `.gate` methods over LinkProperties recursive DNS.
  - Status: verified
  - Evidence: 2026-10-06 физическая пара (moto g54 API35 + A142P API36). Carrier kill -9 → авто-restart (RestartCount+1), msgd PID unchanged, оба телефона reconnect/retry PASS. msgd kill -9 → авто-restart healthy на 172.18.0.3, carrier PID unchanged, обе стороны PASS. msgd-only `--force-recreate --no-deps` → новый container ID, carrier PID unchanged, static IP сохранён, обе стороны PASS. Все три цикла: pairedQueuedSend → crash/recreate → reconnectQueuedAfterServerRestart (byte-identical ciphertext, QUEUED→ACCEPTED, transient transport errors bounded 90s) → peer pairedIncoming received1 then0/skips0 → pairedDeliveredHistoryReopen persistent Delivered. Итог: 17/17 device-method PASS, 0 skips.
- R4: rollout/docs/commit/cleanup
  - Source: «делай копию плана в цель и иеративно реализовать; коммит после, задеплоить правки на прод, прошлое всё сноси … потеря данных не критична».
  - Acceptance: новая topology/parser deployed, obsolete совместный namespace requirement удалено; gates green, intended changes committed, fixtures cleaned.
  - Primary evidence: healthy/schema5/pin/tunnel invariants, diff/docs, git log/status.
  - Status: verified
  - Evidence: backup `snap-1791287385` (db 143360 B) до rollout; `/opt/srv/53` deploy/compose/Dockerfile/patch/env обновлены (старые сохранены в `/opt/srv/53/.local/`); обе службы пересозданы, healthy, schema5; secrets/volumes/pins/nft/туннель не тронуты; приглашения gate удалены, gate-пакеты с телефонов сняты, main identity unchanged; настоящий commit создан после фиксации evidence (см. git log `feat(deploy): independent carrier/msgd recovery`); R9 остаётся deferred_by_user.

### Constraints / non-goals
- Только dmsg53; исходный tunnel/другие containers/global firewall/Docker daemon не менять. Endpoint/static carrier IP сохраняются.
- Secrets readonly files, не Git/env/argv/logs; clean allowlist builds. Server keys/pins/main Android identities сохраняются.
- Разрешение на снос узкое: заменить старую dmsg53 topology. Data loss допустима при необходимости; совместимые данные не требуют wipe. Backup перед rollout.
- Vendor checkout не редактировать; одна shared patch. Без DNS/QUIC rewrite, legacy/migration fallback, watchdog/healthcheck supervisor/cascade restart.
- R9 deferred; mobile/restricted egress/прочие K4 gates не включены. msgctl health не выдавать за end-to-end DNS.

## Change Envelope
- Compose/env-example/Dockerfile.slipstream, SPCDNS overlay/native staging/targeted tests, relevant ARCH/deploy/AUTH/goal evidence.
- Reuse/минимально adjust Android R8 instrumentation для bounded recovery; rebuild native/APK из-за shared parser.
- Remote `/opt/srv/53`: source deploy, новая private-IP env настройка, backup, build/recreate project services, independent crash/recreate experiments. Без global prune/unrelated deletion.
- В requested commit включить наши предыдущие completed R8 test/docs changes; исключить чужой untracked design image и `.local/`.

## Current Checkpoint
- Closes: R1–R4 (все verified).
- Smallest next action: none — контракт закрыт.

## Current State
- Resolved: R1–R4 verified; production работает на независимой topology с патченным парсером.
- Last relevant evidence: 17/17 device-method PASS 0 skips; server healthy/schema5; cleanup подтверждён.
- Blocker: none.
- Next: none в рамках этой цели.

## Material Decisions
- 2026-10-06: fixed backend в existing bridge, не cached hostname/reversed namespace owner.
- 2026-10-06: shared build overlay, не vendor edits. Совместимые данные/ключи сохранить; old topology удалить без fallback.
- 2026-10-06: fish стал root shell на n-de2 — harness-команды обёрнуты в `sh -c`; socat-proxied SSH стабилизирован ControlMaster.
- 2026-10-06: reconnect-гейт удаляет queued-record после retry; mid захватывается локально до verify-фазы.

## Checkpoint History
- 2026-10-06: contract frozen; production ещё не изменён.
- 2026-10-06: R1/R2 реализованы, workspace gates green (parser 12/12 sanitizer, workspace tests).
- 2026-10-06: production rollout (backup → build 32197dab84e2 → recreate), host-level crash probes green.
- 2026-10-06: физическая пара: carrier-crash, msgd-crash, msgd-recreate циклы — все PASS; cleanup; evidence зафиксирован.

## Completion
- Resolved outcomes: R1, R2, R3, R4 — verified.
- Commands and artifacts: см. Evidence у каждого R*; приватные логи/пруфы в gitignored `.local/r8b-independent/`.
- Constraint and diff-scope check: constraints соблюдены (туннель/secrets/pins/volumes/main identity нетронуты; diff в заявленном envelope).
- Final status: complete.
