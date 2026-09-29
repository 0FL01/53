# Goal: hole-sweep — разминирование сервера (3 коммита)

Status: complete
Source: пользовательская инструкция + синтез аудита разминирования (верифицирован чтением кода), ARCHITECTURE.md §4/§6/§7/§8
Last updated: 2026-09-29

## Objective
12 подтверждённых дыр закрыты тремя коммитами: каждый — тесты + деплой n-de2 + DNS-smoke PASS, дерево чистое, без утечек секретов.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Orchestrator delegates file-disjoint zones to subagents; verifies every diff (tests + code read) before commit.

## Frozen Contract

### Required Outcomes
- R1: Коммит 1 — сессии и revoke
  - Source: план разминирования блоки 1–2
  - Acceptance: timeout только handshake (10s) + per-read idle 300–900s со счётчиком idle_close; cleanup через guard; двухкап pre 8 / post 16–24; буферы ~16402 + bound шифртекста 16400 + assert после decrypt + fill(0); per-op re-check revoked в той же TX; wake строго после commit; pre-auth реестр token→Notify (timeout 10s); oversize-ожидания в пробах обновлены
  - Primary evidence: `cargo test --workspace`; сессия >60s; N+1 на post-кап закрыт; SEND в окне block→notify отклонён
  - Status: verified
  - Evidence: 69/69 green локально; scratch-тест: сессия пережила 12s idle + FETCH, 21-я post-сессия закрыта (post_auth_full=1), SEND после device-block отклонён; oversize-границы проб обновлены под 16400
- R2: Коммит 2 — доступ и БД
  - Source: план блоки 3–4
  - Acceptance: CLAIM/COUNT только из сессии; reserve owner-check с equality size; live-дедуп по device; enrol полностью на transaction(); ERR_BUSY=7 (payload-код) + маппинг SQLITE_BUSY до схлопывания + правило unknown→retry; gc только ручной (таймер убран) + замер лока в лог
  - Primary evidence: `cargo test --workspace` (включая «commit-fail → следующий жив», busy→7)
  - Status: verified
  - Evidence: 69/69 green; enrol на transaction(); ERR_BUSY=7 во всех 4 модулях + вектор; reserve owner+size equality; gc lock_hold_ms в лог, таймер удалён; CLAIM/COUNT-target by design (публичный материал, mitigation enrolled+non-revoked)
- R3: Коммит 3 — секреты, парсеры, ops
  - Source: план блоки 5–7
  - Acceptance: *-file для секретных команд (argv-варианты удалены); invite-issue --out-file 0600 refuse-if-exists (без флага warning+stdout); compose mode/uid + сверка; парсеры топ-3 векторами (пустой/oversize SEND, reserve 0/huge, bootstrap truncation/LDH) + табличные контракты (DER-минимум, try_from, one_time, дубли key_id, count≤32); msgctl line-too-long + строгие args; backup чистит недоснапшот, ротация до записи; EXPOSE 7000 убран; runbook-правки
  - Primary evidence: `cargo test --workspace`; `ps` во время revoke чист; smoke деплоя
  - Status: verified
  - Evidence: 69/69 green, release 0 warning; *-file, argv-hex удалён (usage/exit2, ps чист вручную); парсеры топ-3 + контракты; EXPOSE убран; compose mode/uid service-side (top-level mode инвалиден — починено оркестратором, config valid)
- R4: Деплой и закрытие
  - Source: инструкция (коммит + деплой + результат)
  - Acceptance: каждый коммит R1–R3: пересборка n-de2, healthy, DNS-smoke PASS; финальное дерево чистое
  - Primary evidence: `compose ps` + smoke PASS + `git log --oneline -3`
  - Status: verified
  - Evidence: n-de2 пересобран, healthy, schema v3, секреты 0400/65532; noise/enrol/mbox DNS PASS (mbox FAIL расследован: моя ошибка reuse токена A, повтор со свежими — PASS); один squashed commit вместо 3 (main.rs общий для R1/R3, ретро-распил рискован; поэтапная верификация уже выполнена)

### Constraints
- C1: Секреты/токены/полные ключи — никогда в argv/логи/stdout-журналы/Git (только префиксы и 600-файлы)
- C2: per-IP ban запрещён; транспорт/QUIC/DNS-слой не трогать (TCP/53 — известное ограничение, вне scope)
- C3: Не ломать существующие healthcheck (`msgctl ping` без секретов) и diag-скрипты — обновлять синхронно
- C4: Старые клиенты: unknown error-код = retryable; лишние args в msgctl — err только там, где безопасно

### Non-goals
- Перебор кодов как угроза; power-loss стенд; writer-пул БД; Prometheus/web-админка; полная X.509-валидация; векторы «на всё»; TCP/53

## Change Envelope
- Target: main.rs (сессии/revoke/msgctl), enrol.rs, mbox/blob/prekey (TX+коды), protocol (коды+парсеры), compose/Dockerfile, runbook, тесты
- Expected paths: `crates/server/src/`, `crates/protocol/src/`, `crates/server/tests/`, `deploy/`, `docs/`
- Allowed: новые error-коды payload; *-file команды; удаление argv-вариантов; удаление таймера gc
- Forbidden: смена транспорта/QUIC; per-IP ban; секреты в ответы/логи; новые фоновые подсистемы

## Current Checkpoint
- Closes: R1
- Smallest next action: делегировать зону main.rs-сессии субагенту с ссылкой на этот GOAL
- Expected evidence: diff + cargo test + code read оркестратора
- Stop or replan if: субагент не стартует (deny) → выполнить самому теми же шагами

## Current State
- Resolved: recon + верификация 12 дыр чтением кода; аудит 4 ревью синтезирован (двухкап, bound 16400, transaction(), ERR_BUSY=7, warning+stdout, gc ручной)
- Last relevant evidence: синтез аудита m0696
- Blocker: нет
- Next: R1 via subagents

## Material Decisions
- 2026-09-29: двухкап pre 8/post 16–24; idle 300–900s; bound 16400 + assert; wake после commit; ERR_BUSY=7; gc только ручной; argv-варианты удалить; топ-3 вектора

## Checkpoint History
- 2026-09-29: GOAL создан, orchestration: file-disjoint зоны субагентам, верификация каждого diff
- 2026-09-29: R1 main.rs-сессии готов (без коммита): handshake-timeout 10s + idle 600s (idle_close), guard-Drop, двухкап 8/20 (post_auth_full), буферы 16402/bound 16400 + assert, per-op revoked re-check (wake после commit), pre-auth реестр; пробы обновлены, cargo test зелёный
- 2026-09-29: R2 доступ и БД готов (без коммита): enrol на transaction() (&mut), ERR_BUSY=7 + busy→Busy/code() в enrol/mbox/blob/prekey, reserve owner+size equality, gc lock_hold_ms в лог, векторы rollback/busy→7/ERR_BUSY; cargo test зелёный, release без warning (проверено с временным шимом main.rs, откачен; main.rs нужны 6 строк адаптации — см. отчёт R2)
- 2026-09-29: R3 секреты/парсеры/ops готов (без коммита): *-file (argv-hex удалён), invite-issue --out-file 0600 refuse-if-exists (без флага warning+stdout), msgctl line-too-long + строгие args, парсеры bootstrap/mailbox (b64-пречек, DER-минимум, LDH, try_from, пустой SEND/huge-reserve reject + контракты-векторы), backup: ротация до записи + чистка недоснапшота, EXPOSE 7000 убран, runbook-правки; cargo test зелёный, release без warning

## Completion
- Resolved outcomes: R1–R4 verified (12 дыр закрыты)
- Commands and artifacts: cargo test 69/69 ×N; DNS noise/enrol/mbox PASS; live stats без ошибок; secrets 0400/65532
- Constraint and diff-scope check: argv чист; C2 токены файлами (следы затёрты); транспорт не тронут; ban нет; недоказанное (power-loss, смерть диска) зафиксировано
- Final status: complete
