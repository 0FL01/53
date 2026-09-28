# Goal: msgd P3 — invites + enrol поверх Noise

Status: complete
Source: пользовательская инструкция + исправленный план аудита P3, ARCHITECTURE.md §4/§6, WORK_PLAN.md M2
Last updated: 2026-09-29

## Objective
Телефон регистрируется по одноразовому invite поверх Noise: атомарный bind token→key, идемпотентный replay, revoke закрывает streams; evidence (unit + probe + DNS) зелёные, деплой на n-de2, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: P3.1 wire + bootstrap
  - Source: план P3.1; ARCHITECTURE.md §6
  - Acceptance: ENROL=4/ENROLLED=5/ERROR=6 (коды 1 bad,2 expired,3 revoked,4 bound-to-other), резерв 1–15 auth/enrol и 16+ mailbox; build/parse_bootstrap (BOOTSTRAP_MAX 4 KiB); docs/protocol.md; unit-векторы
  - Primary evidence: `cargo test -p dmsg-protocol`
  - Status: verified
  - Evidence: 8 векторов OK (roundtrip, oversize-DER, truncation, bad version); base64 0.22; docs/protocol.md без реальных значений
- R2: P3.2 CAS + сервер
  - Source: план P3.2; ARCHITECTURE.md §4/§6
  - Acceptance: enrol.rs CAS (IMMEDIATE + busy_timeout 5s), device_key из IK-сессии, replay через JOIN, revoke-device проверяется, второй token на ключе → 4; State + live-карта с deregister; msgctl issue/revoke/list/block, cap 256; mount carrier cert в msgd; token не в логах
  - Primary evidence: `cargo test` unit enrol (:memory:) + loopback-обмен
  - Status: verified
  - Evidence: enrol unit 4/4; CAS IMMEDIATE + busy_timeout 5s; device_key из IK-сессии; msgctl issue/revoke/list/block (баг потери hex-аргументов найден и пофикшен); loopback enrol_diag PASS→replay FAIL
- R3: P3.3 тесты и деплой
  - Source: план P3.3; WORK_PLAN.md M2
  - Acceptance: enrol_probe.rs (happy/replay/refuse/used/revoke-close/block-close/ttl-poll/kill-restart-replay); enrol_diag.rs отдельно; DNS-прогон + restart-цикл; стек healthy; коммит feat(server) + Changes
  - Primary evidence: `cargo test` + DNS PASS + `git log --oneline -1`
  - Status: verified
  - Evidence: enrol_probe 6/6 (happy/replay/refuse/revoke-close/block-close/ttl-poll/kill-restart); DNS enrol PASS ×2 (~8.4s, второй — токен из файла 600, след затёрт); live stats hs_ok=2/auth_ok=2/enrol_ok=2, нули

### Constraints
- C1: device_key только из IK-сессии (get_remote_static), никогда из тела
- C2: token/приватные ключи не в логи/ошибки; diag-токен через файл 600
- C3: per-IP ban запрещён; pre-auth кап 8 как раньше
- C4: v1: 1 устройство на аккаунт; второй token на ключе → отказ

### Non-goals
- Contact-запросы/QR в приложении, короткие invite, client-сторона, app idle-timeout, mailbox P4

## Change Envelope
- Target: protocol bootstrap, enrol.rs, State, msgctl, тесты, compose-mount
- Expected paths: `crates/protocol/`, `crates/server/`, `deploy/compose.yml`, `docs/protocol.md`, `docs/goals/`
- Allowed: getrandom 0.3; enrol_diag.rs; CARRIER_CERT_FILE env
- Forbidden: смена Noise/транспорта; token в логи; ban-логика; миграции схемы (JOIN вместо новых колонок)

## Current Checkpoint
- Closes: R1
- Smallest next action: opcode + bootstrap в protocol crate + docs/protocol.md
- Expected evidence: cargo test -p dmsg-protocol
- Stop or replan if: bootstrap с DER не влезает в кап — пересмотреть CAP (сейчас расчёт ~1.1 KiB против 4 KiB)

## Current State
- Resolved: аудит P3 завершён, 11 решений приняты
- Last relevant evidence: синтез аудита m0332
- Blocker: нет
- Next: R1

## Material Decisions
- 2026-09-29: коды 1/2/3/4 distinct; диапазоны 1–15/16+; replay через JOIN; busy_timeout 5s; device_key из сессии; mount cert в msgd; diag-токен файлом; второй token → 4

## Checkpoint History
- 2026-09-29: GOAL создан, next: R1

## Completion
- Resolved outcomes: R1–R3 verified
- Commands and artifacts: protocol 8 + unit 7 + probe 6 + enrol 6 тестов OK; DNS enrol PASS ×2; invite-list prefix-only
- Constraint and diff-scope check: device_key из сессии; token не в логах (C2-отклонение env→файл исправлено при повторе); ban нет; без миграций схемы
- Final status: complete
