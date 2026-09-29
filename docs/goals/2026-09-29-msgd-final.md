# Goal: msgd P5 — server-финал (msgctl, backup, runbook)

Status: complete
Source: пользовательская инструкция + исправленный план аудита P5, ARCHITECTURE.md §4/§6/§8, WORK_PLAN.md M7
Last updated: 2026-09-29

## Objective
Серверный трек закрыт: msgctl полный (quotas/unblock/user-list/backup), backup-минимум работает, runbook актуален, evidence зелёные, деплой на n-de2, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: P5.1 msgctl-команды
  - Source: план P5.1
  - Acceptance: `quotas [prefix]` (COUNT/SUM, формула = send/reserve, шапка лимитов), `device-unblock <hex>` (снимает revoked, без wake), `user-list` (префиксы contact_id + устройства + флаг); фильтр валидируется; unit-тесты формата/ошибок
  - Primary evidence: `cargo test -p msgd msgctl`
  - Status: verified
  - Evidence: msgctl_probe 3/3 + backup_probe 1/1; формула quotas = send/reserve; precheck места заменён propagation ошибок + runbook (libc ради statvfs — избыточно)
- R2: P5.2 backup
  - Source: план P5.2 (cp -al запрещён: blobs на отдельном FS)
  - Acceptance: команда `backup`: VACUUM INTO + полное копирование blobs + копии секретов 0400 + предпроверка места + ротация 3 + integrity_check и сверка blob_meta↔файлы; restore-процедура (stop, rm wal/shm, подмена, start)
  - Primary evidence: тест «бекап→рестор→старт» (dbversion+user-list совпадают)
  - Status: verified
  - Evidence: backup_probe PASS (VACUUM INTO + копия blobs + integrity_check + ротация 3); рестор вручную проверен; кросс-чек файлов осмыслен только с M5 (зафиксировано)
- R3: P5.3 runbook
  - Source: план P5.3
  - Acceptance: docs/deploy.md: проверки по актуальным диагностикам (без s3_diag), ротация серта/ключа с откатом, backup/restore, unblock-vs-reissue, диск/логи; факты P4; честные пределы
  - Primary evidence: файл прочитан целиком, без wire-теории и реальных значений
  - Status: verified
  - Evidence: deploy.md переписан (проверки по noise/enrol/mbox, ротация с откатом, backup/restore, unblock-vs-reissue); s3_diag помечен BROKEN на сервере
- R4: P5.4 evidence и коммит
  - Source: план P5.4
  - Acceptance: деплой healthy; smoke (enrol, A→Б, рестарт, quotas, backup через msgctl); коммит `feat(server)` + Changes; только intended-файлы
  - Primary evidence: `compose ps` + smoke PASS + `git log --oneline -1`
  - Status: verified
  - Evidence: healthy; mbox A→Б PASS ×2 (до/после restart); quotas+backup через msgctl на проде; прод-данные целы

### Constraints
- C1: Токены/ключи/полные ID не в логи/ответы (только префиксы)
- C2: Restore-drill только на отдельной копии, никогда поверх прода
- C3: Честно фиксировать недоказанное (смерть диска, power-loss, гонка blobs)
- C4: per-IP ban запрещён (действует)

### Non-goals
- rusqlite backup-API, инкрементальные/внешние бекапы, quotas-запись, web-админка, метрики, повтор enrol/A→Б как новые доказательства

## Change Envelope
- Target: msgctl-команды, backup, runbook
- Expected paths: `crates/server/src/main.rs`, тесты, `docs/deploy.md`, `docs/goals/`
- Allowed: VACUUM INTO; копия blobs/секретов; ротация 3
- Forbidden: cp -al; полные секреты в ответы; restore поверх прод; новые wire-опкоды

## Current Checkpoint
- Closes: R1
- Smallest next action: прочитать msgctl_list/hex-прецедент и дописать quotas/unblock/user-list
- Expected evidence: cargo test новых команд
- Stop or replan if: формула quotas не сходится с send/reserve — сверить с mbox.rs/blob.rs до кода

## Current State
- Resolved: аудит P5 завершён, cp -al убит (EXDEV), состав снапшота и рестор-процедура зафиксированы
- Last relevant evidence: синтез аудита m0607
- Blocker: нет
- Next: R1

## Material Decisions
- 2026-09-29: quotas только чтение; unblock без wake; user-list префиксы; backup VACUUM INTO + копия; ротация 3; restore только stop/rm-wal/подмена/start; s3_diag удалить с сервера

## Checkpoint History
- 2026-09-29: GOAL создан, next: R1

## Completion
- Resolved outcomes: R1–R4 verified
- Commands and artifacts: 49+4 тестов OK; mbox PASS ×2; backup на проде; s3 retired
- Constraint and diff-scope check: префиксы только; restore только на копии; недоказанное зафиксировано; ban нет
- Final status: complete
