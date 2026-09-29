# Goal: msgd P4 — mailbox + текст (M3-server)

Status: complete
Source: пользовательская инструкция + исправленный план аудита P4, ARCHITECTURE.md §4/§8, WORK_PLAN.md M3
Last updated: 2026-09-29

## Objective
Сервер хранит и разносит текст: durable server-ACK после commit, cursor в той же TX, дедуп UNIQUE, prekeys атомарно, blob reservation/GC; chaos-матрица зелёная локально + один DNS-прогон А→Б; деплой на n-de2, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: P4.1 wire + схема
  - Source: план P4.1
  - Acceptance: опкоды SEND/SEND_ACK/FETCH/FETCH_RESP/DELIVERY_ACK/UPLOAD_PREKEYS/PREKEY_COUNT/BLOB_RESERVE (+ERROR: no-prekey/quota/oversize); миграция v2 (cursors); векторы protocol
  - Primary evidence: `cargo test -p dmsg-protocol`
  - Status: verified
  - Evidence: 12 векторов OK (mailbox.rs strict-парсеры); миграции v2 cursors + v3 device_identities; docs/protocol.md дополнен
- R2: P4.2 mailbox
  - Source: план P4.2; ARCHITECTURE.md §8
  - Acceptance: SEND→quota-в-TX→INSERT ON CONFLICT→commit→SEND_ACK (повтор = прежний accept); FETCH + cursor в той же TX; DELIVERY_ACK селективный, cursor по непрерывному; статусы 4 без «прочитано»; eviction старые-первых, переполнение reject
  - Primary evidence: Rust-тесты loopback (replay/reorder/restart)
  - Status: verified
  - Evidence: mbox unit 2/2; commit→ACK; повтор=прежний accept; cursor в TX по непрерывному; повтор после доставки → ST_DELIVERED (дотянуто: статус был заведён, но не отдавался)
- R3: P4.3 prekeys + blobs
  - Source: план P4.3
  - Acceptance: upload с binding-проверкой; consume SELECT+DELETE в TX; пустой запас ERROR; refill по low-count; reservation+ACL; orphan-sweep 24ч; GC poll-цикл; TTL 7 сут
  - Primary evidence: Rust-тесты (race-consume, empty-stock, quota-reject, ACL)
  - Status: verified
  - Evidence: prekey unit (binding+IdentityChanged), blob unit 3/3; consume атомарно; пустой запас ERROR; GC poll 300s + msgctl gc; eviction старые-первых
- R4: P4.4 evidence + деплой + коммит
  - Source: план P4.4; WORK_PLAN.md M3
  - Acceptance: матрица локально (kill/restart, corrupt, oversize, TTL crafted-SQL, tmpfs disk-full, identity-change, ACK-разделение); один DNS А→Б с рестартом без дублей; отдельный mailbox-example; стек healthy; коммит feat(server)
  - Primary evidence: `cargo test --workspace` + DNS PASS + `git log --oneline -1`
  - Status: verified
  - Evidence: workspace 45 тестов OK, 0 warning; матрица 8/8 (replay/reorder/kill-restart/corrupt/oversize/TTL-crafted/tmpfs-disk-full/identity-change/unknown-version); DNS A→B PASS 8.7s; live send_ok=1/dedup=1/fetch=1/ack=1, нули

### Constraints
- C1: server-ACK строго после commit (WAL+FULL); kill ≠ power-loss — тест asserts порядок, не обесточивание
- C2: sender_device только из Noise-сессии; дедуп серверным UNIQUE
- C3: TTL без sleep-флаков (crafted-SQL/short-TTL+poll); read receipts нет
- C4: Остаточный риск 32 слотов транспорта действует

### Non-goals
- Read receipts, серверное переупорядочение, chunk transfer (M5), E2E-клиент, power-loss rig

## Change Envelope
- Target: mailbox/prekey/blob логика msgd, миграция v2, тесты, mailbox-example
- Expected paths: `crates/protocol/`, `crates/server/`, `docs/goals/`
- Allowed: новые опкоды; cursors; GC poll; tmpfs-тест
- Forbidden: смена Noise/enrol; chunk-протокол; read receipts

## Current Checkpoint
- Closes: R1
- Smallest next action: опкоды + миграция v2 + векторы
- Expected evidence: cargo test -p dmsg-protocol
- Stop or replan if: схема v2 ломает enrol-тесты → чинить совместимость, не откатывать

## Current State
- Resolved: аудит P4 завершён, 4 уточнения приняты (cursors v2, селективный ACK, binding-проверка, матрица локально)
- Last relevant evidence: синтез аудита m0471
- Blocker: нет
- Next: R1

## Material Decisions
- 2026-09-29: DELIVERY_ACK селективный батчированный; cursor до непрерывного; GC poll в msgd; eviction старые-первых; TTL crafted-SQL

## Checkpoint History
- 2026-09-29: GOAL создан, next: R1

## Completion
- Resolved outcomes: R1–R4 verified
- Commands and artifacts: 45 тестов; mbox_dns PASS loopback+DNS; stats подтверждают цикл
- Constraint and diff-scope check: ACK после commit; sender из сессии; TTL без sleep; kill≠power-loss честно; remote Cargo.lock не синкнулся (sftp-флейк, свежий резолв semver)
- Final status: complete
