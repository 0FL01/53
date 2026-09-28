# Goal: Бэкенд MVP — транспорт, compose, msgd-stub, деплой n-de2

Status: complete
Source: пользовательская инструкция 2026-09-28 + исправленный план аудита (m0026), ARCHITECTURE.md §1/§4, WORK_PLAN.md M0/M1
Last updated: 2026-09-28

## Objective
Бэкенд поднимается на n-de2 из одного пина транспорта: один домен из `.env`, путь только через TCP-резолвер, msgd-stub отвечает `WELCOME`, проверки зелёные, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: GOAL-документ заморожен
  - Source: инструкция «копию исправленного плана в goal»
  - Acceptance: файл `docs/goals/2026-09-28-backend-mvp.md` в статусе active с R1–R6
  - Primary evidence: файл существует в Git-рабочем дереве
  - Status: in_progress
  - Evidence:
- R2: S0 — пин транспорта + скелет deploy + baseline
  - Source: исправленный план S0; WORK_PLAN.md M0
  - Acceptance: `vendor/slipstream` — submodule @d7cd555; `project/deploy/{Dockerfile.slipstream,compose.yml,.env.example}` без секретов; `project/docs/baseline.md` с агрегатами
  - Primary evidence: `git submodule status`, `meson test -C <build> --print-errorlogs` pass из пина
  - Status: verified
  - Evidence: submodule `d7cd555` + nested SHAs; `meson test` 4/4 OK (runtime ~34s, shared build, conda openssl headers + LD_PRELOAD system libstdc++); sanitizer отложен (нет libasan) — см. project/docs/baseline.md
- R3: S1 — compose slipstream+msgd
  - Source: исправленный план S1; ARCHITECTURE.md §4
  - Acceptance: `network_mode: service:slipstream`, msgd на `127.0.0.1:7000`, наружу только DNS-порт, secrets ro-mounts, non-root, limits, joint recreate; зафиксированы max-connections и idle-timeout
  - Primary evidence: `docker compose -f project/deploy/compose.yml config` валиден
  - Status: verified
  - Evidence: `docker compose --env-file project/deploy/.env.example config` рендерит оба сервиса, max-connections=32, idle-timeout=30, DNS-порт наружу единственный
- R4: S2 — msgd-stub + msgctl-stub
  - Source: исправленный план S2
  - Acceptance: Rust/Tokio TCP `127.0.0.1:7000`, `HELLO → WELCOME(DMSG_DOMAIN)`, opcode version + frame ≤16 KiB + close на oversize/unknown, mismatch → close+counter (без ban); `msgctl`: `ping/domain` по Unix socket
  - Primary evidence: локальный обмен HELLO/WELCOME + отказ на чужой домен
  - Status: verified
  - Evidence: good→WELCOME; mismatch/badver/badop→close без ban; msgctl ping/domain/bogus→pong/domain/err; пустой DMSG_DOMAIN→exit 2
- R5: S3 — диагностика + деплой + тесты
  - Source: исправленный план S3; WORK_PLAN.md M1
  - Acceptance: 2 native-клиента через recursive TCP обмениваются байтами через msgd-stub; рестарты без роста RSS/FD; `/opt/srv/53` на n-de2 поднят из `.env`
  - Primary evidence: лог диагностики + `compose ps` на n-de2
  - Status: verified
  - Evidence: s3_diag 2/2 PASS до и после restart (~1.2s); msgd healthy; recursive TCP через 1.1.1.1 отвечает (NXDOMAIN≠SERVFAIL); runbook project/docs/deploy.md
- R6: коммит
  - Source: инструкция «потом коммит»; AGENTS.md Commits
  - Acceptance: коммит `<type>(<scope>): <desc>` + `Changes:` 2–4 буллета; `git status` — только intended-файлы
  - Primary evidence: `git log --oneline -1` + `git status --short --branch`
  - Status: verified
  - Evidence: `f9d5b17 feat(backend): msgd-stub, compose и деплой S0–S3`, дерево чистое

### Constraints
- C1: Не трогать рабочее развёртывание туннеля; отдельный поддомен/IP/сервер (ARCHITECTURE.md §1)
- C2: Секреты только read-only файлами, не в Git/образ/логи/argv; данные в volumes (ARCHITECTURE.md §4)
- C3: Не переписывать DNS/QUIC/congestion/scheduler; серверный UDP-сокет не трогать
- C4: `.local/`, `.opencode/`, `opencode.jsonc` не коммитить (.gitignore)
- C5: UDP-резолверы и UDP↔TCP fallback вне scope; числа адаптера 56/64 слепо не переносить

### Non-goals
- S4 SQLite (следующая цель, не эта итерация)
- Noise/Olm/E2E, полный msgctl, HA/Redis/PG/K8s, звонки, TCP:53 на сервере

## Change Envelope
- Target: deploy-скелет, compose, msgd-stub
- Expected paths, symbols, and direct consumers:
  - `vendor/slipstream` (submodule), `project/deploy/`, `project/crates/server/`, `project/docs/`, `docs/goals/`
  - потребители: `docker compose` на n-de2, `msgd` ← `slipstream --target-address`
- Allowed and forbidden artifacts:
  - Allowed: submodule, Dockerfiles без секретов, compose.yml, .env.example (плейсхолдеры), Rust-stub, baseline.md, runbook-минимум
  - Forbidden: секреты/реальные домены-IP в Git, COPY certs/ в образ, Python-адаптер в образ, UDP-fallback, новые зависимости сверх Tokio
- User or harness budget: без лимитов по путям сверх C4; деплой только на n-de2:/opt/srv/53

## Current Checkpoint
- Closes: R1
- Smallest next action: записать этот файл, затем S0: `git submodule add -b feat/rust-parity-ab <url> vendor/slipstream && git -C vendor/slipstream checkout d7cd555`
- Expected evidence: файл существует; далее `git submodule status`
- Stop or replan if: remote submodule недоступен → зафиксировать точную ошибку и smallest unlock

## Current State
- Resolved: аудит плана завершён (4 ревью синтезированы), противоречия разрешены
- Last relevant evidence: m0026 — исправленный минимальный план
- Blocker: нет
- Next: записать GOAL → S0

## Material Decisions
- 2026-09-28: submodule (не subtree); `.env` живёт на n-de2:/opt/srv/53, в Git только `.env.example`; TCP-first на плече клиент→резолвер, серверный UDP-сокет не трогать; stub→SQLite; mismatch → close+counter без ban; baseline M0 на bench-бэкенде :40100, интеграция M1 на stub :7000

## Checkpoint History
- 2026-09-28: аудит (R-нет, план), 4 ревью, синтез m0026, next: GOAL+S0

## Completion
- Resolved outcomes: R1–R6 verified (GOAL, пин+гейты, compose config, stub-обмен, S3 PASS+рекурсия, коммит f9d5b17)
- Commands and artifacts: `meson test` 4/4 OK; `compose config` OK; HELLO/WELCOME + msgctl; s3_diag 2/2 ×2; `dig +tcp` NXDOMAIN≠SERVFAIL
- Constraint and diff-scope check: туннель не тронут; секретов/реальных имён-IP в Git нет; UDP-сокет не менялся; 13 файлов, только intended
- Final status: complete
