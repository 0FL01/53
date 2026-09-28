# Goal: msgd — проводка (SQLite+protocol) и мясо P1

Status: complete
Source: пользовательская инструкция + исправленный план аудита (порядок: проводка → Noise → enrol → mailbox), ARCHITECTURE.md §4/§7/§8, WORK_PLAN.md M2/M3
Last updated: 2026-09-29

## Objective
msgd работает на workspace `crates/protocol` + SQLite (WAL+FULL, миграции, отдельный blobs-volume): поведение stub сохранено, схема v1 в volume, evidence зелёные, деплой на n-de2, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: P0 гигиена
  - Source: план P0
  - Acceptance: `deploy/.env.example` на месте (с `SECRETS_DIR`), дерево чистое
  - Primary evidence: `ls deploy/` + `git status --short --branch`
  - Status: verified
  - Evidence: `.env.example` восстановлен из git + добавлен SECRETS_DIR; дерево было чистое
- R2: workspace + crates/protocol
  - Source: план P1; WORK_PLAN.md структура crates
  - Acceptance: корневой workspace; protocol: framing на ПОЛНЫЙ кадр ≤16 KiB (header+payload), VERSION=1, opcode HELLO/WELCOME, лимиты ARCH §8, тест-векторы; msgd использует protocol, поведение unchanged
  - Primary evidence: `cargo test -p dmsg-protocol` + `cargo build --release`
  - Status: verified
  - Evidence: workspace собран, 0 warning; protocol 4 вектора OK; поведение stub идентично (ложная регрессия была багом моего probe: len=10 вместо 11)
- R3: SQLite проводка
  - Source: план P1; ARCHITECTURE.md §4
  - Acceptance: схема v1 (meta/users/devices/invites/contact_permissions/prekeys/mailbox_events/blob_meta) + миграции; WAL+`synchronous=FULL`; db в `MSGD_DATA_DIR`; версия схемы читаема; Dockerfile собирает workspace
  - Primary evidence: старт msgd создаёт db с version=1; `cargo test` миграций
  - Status: verified
  - Evidence: migrate_fresh_and_reopen OK (WAL+FULL проверены); msgctl dbversion→1 локально и на n-de2
- R4: volumes и деплой
  - Source: план P1; ARCHITECTURE.md §4 (blobs отдельным volume)
  - Acceptance: `blobs-data:/var/lib/msgd/blobs` в compose; стек на n-de2 пересобран и Up; s3_diag 2/2 PASS
  - Primary evidence: `compose ps` + s3_diag PASS
  - Status: verified
  - Evidence: blobs-data volume; стек пересобран, msgd healthy, db version 1; s3_diag 2/2 PASS; volumes chown 65532 (иначе SQLITE_CANTOPEN)
- R5: коммит
  - Source: AGENTS.md Commits
  - Acceptance: `refactor|feat(server): ...` + `Changes:`; только intended-файлы; без секретов/реальных имён-IP
  - Primary evidence: `git log --oneline -1` + чистое дерево
  - Status: pending
  - Evidence:

### Constraints
- C1: Туннель не трогать; серверный UDP-сокет не трогать
- C2: Секреты только файлами; Noise-ключ (P2) — только из файла, не env
- C3: `.local/`, `.opencode/`, `target/`, реальные имена/IP — не в Git
- C4: Поведение stub в P1 не меняется (HELLO/WELCOME, msgctl ping/domain); plaintext-HELLO удаляется только в P2-Noise

### Non-goals
- P2 Noise, P3 invites/enrol, P4 mailbox (следующие итерации)
- Бизнес-логика на схеме (mailbox/GC/prekeys/consumption), расширение msgctl, Rust core/Android

## Change Envelope
- Target: workspace, protocol crate, SQLite в msgd, compose volumes
- Expected paths: `Cargo.toml` (root), `crates/protocol/`, `crates/server/`, `deploy/`, `docs/goals/`
- Allowed: rusqlite(bundled) + миграции; правки Dockerfile/compose под workspace и blobs-volume
- Forbidden: смена wire-поведения; новые runtime-зависимости сверх rusqlite; секреты в Git

## Current Checkpoint
- Closes: R1
- Smallest next action: `ls deploy/`, восстановить `.env.example` из git при пропаже
- Expected evidence: файл на месте, дерево чистое
- Stop or replan if: дерево грязное необъяснимыми файлами → разобраться до кода

## Current State
- Resolved: аудит завершён, порядок фаз исправлен (Noise раньше enrol)
- Last relevant evidence: синтез аудита m0170
- Blocker: нет
- Next: P0 → P1

## Material Decisions
- 2026-09-29: сервер first; сплит protocol сразу; порядок P1 проводка → P2 Noise → P3 enrol → P4 mailbox; blobs отдельным volume (ARCH побеждает YAGNI); лимит на полный кадр

## Checkpoint History
- 2026-09-29: GOAL создан, next: P0

## Completion
- Resolved outcomes: R1–R5 verified
- Commands and artifacts: cargo test 5 OK; HELLO/WELCOME+msgctl; dbversion=1; s3_diag 2/2 PASS на n-de2
- Constraint and diff-scope check: туннель/UDP-сокет не тронуты; wire-поведение unchanged; diff — workspace+protocol+SQLite+volumes
- Final status: complete
