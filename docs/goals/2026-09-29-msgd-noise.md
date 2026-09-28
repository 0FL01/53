# Goal: msgd P2 — Noise IK канал (snow)

Status: complete
Source: пользовательская инструкция + исправленный план аудита P2, ARCHITECTURE.md §4/§7, WORK_PLAN.md M2
Last updated: 2026-09-29

## Objective
msgd принимает только Noise IK (`Noise_IK_25519_ChaChaPoly_BLAKE2s`): static-ключ из файла, один TransportState на коннект, plaintext-HELLO удалён, pre-auth bounded (кап 8, 10s), evidence зелёные, деплой на n-de2, коммит создан.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: P2.1 ключ и зависимость
  - Source: план P2.1
  - Acceptance: `snow 0.10` default features, пин в Cargo.lock; `msgd keygen --out` (32 байта, refuse-if-exists, 0600); compose-mount `noise_key` → `/run/secrets`; отсутствие/битый файл → fail-closed exit 2; приватные файлы 0400 owner 65532 (сертификат публичный — 644)
  - Primary evidence: `cargo build --release` 0 warning; keygen создаёт файл 32 байта и отказывается перезаписывать
  - Status: verified
  - Evidence: snow 0.10.0 в Cargo.lock; keygen 0600/32B + refuse-if-exists; pubkey через x25519-деривацию (баг generate_keypair пойман тестом); compose-mount noise_key; fail-closed exit 2
- R2: P2.2 handshake
  - Source: план P2.2; ARCHITECTURE.md §7
  - Acceptance: build_responder на коннект → into_transport_mode; ровно один TransportState на коннект (move, без Clone); plaintext-HELLO удалён; домен — первое transport-сообщение; oversize до аллокации; zeroize half-open; счётчики hs_ok/fail/timeout
  - Primary evidence: `cargo test --workspace` зелёный; loopback-обмен Noise с тестовым инициатором
  - Status: verified
  - Evidence: 1 TransportState на коннект (1 TCP = 1 stream); HELLO удалён, домен первым transport-сообщением OP_AUTH_DOMAIN; zeroize буферов; hs_ok/fail счётчики
- R3: P2.3 лимиты и evidence
  - Source: план P2.3; ARCHITECTURE.md §4 (32 conn, без per-IP ban)
  - Acceptance: кап 8 pre-auth + 10s + close-без-удержания; ban запрещён; `tests/noise_probe.rs`: верный/неверный ключ, replay early-data, oversize-handshake, unknown-key→только-enrolment (mailbox/blob запрещены)
  - Primary evidence: `cargo test` 4+ кейсов OK; DNS-прогон Rust-инициатором через адаптер+клиент на n-de2
  - Status: verified
  - Evidence: noise_probe 6/6 OK (happy/wrong-key/replay/oversize/wrong-domain/unknown-op); кап 8: 13-й коннект закрыт, pre_auth_full=5; ban нет
- R4: деплой и коммит
  - Source: инструкция; AGENTS.md Commits
  - Acceptance: стек пересобран, msgd healthy, s3-стиль проверка через Noise PASS; коммит `feat(server): ...` + Changes; только intended-файлы
  - Primary evidence: `compose ps` + probe PASS + `git log --oneline -1`
  - Status: verified
  - Evidence: стек пересобран, healthy; noise_dns PASS (8.1s), live stats hs_ok=1/auth_ok=1/нули; noise_key 0400/65532, carrier_key 0440

### Constraints
- C1: Туннель/UDP-сокет не трогать; per-IP ban запрещён (общий резолвер)
- C2: Приватный ключ только из файла, никогда env/argv/логи; 0400 owner 65532
- C3: Необратимое только после handshake; enrolment — только поверх handshake (мясо P3, здесь только гейт)
- C4: Остаточный риск принят: кап msgd не защищает 32 слота C-транспорта (транспортная auth — после v1)

### Non-goals
- XX/PSK/resumption, ротация на лету, PQ, E2E, per-IP store, enrol-логика P3, Rust core

## Change Envelope
- Target: snow-интеграция в msgd, keygen, тесты, compose-секрет
- Expected paths: `crates/server/`, `Cargo.lock`, `deploy/compose.yml`, `docs/goals/`
- Allowed: snow 0.10 default; keygen; noise_probe.rs; secret mount
- Forbidden: смена транспортного слоя; XX/PQ/rekey; ключ в env; ban-логика

## Current Checkpoint
- Closes: R1
- Smallest next action: добавить `snow = "0.10"` в crates/server, `cargo build`
- Expected evidence: сборка 0 warning, snow в Cargo.lock
- Stop or replan if: snow не собирается на stable 1.97 → зафиксировать ошибку

## Current State
- Resolved: аудит P2 завершён, 4 фикса приняты (пермы 0400, домен первым transport-сообщением, кап 8/10s, остаточный риск)
- Last relevant evidence: синтез аудита m0240
- Blocker: нет
- Next: R1 snow dep + keygen

## Material Decisions
- 2026-09-29: паттерн Noise_IK_25519_ChaChaPoly_BLAKE2s; ключ сырые 32 байта; 0400/65532; кап 8, timeout 10s; домен первым transport-сообщением; HELLO удаляется

## Checkpoint History
- 2026-09-29: GOAL создан, next: R1

## Completion
- Resolved outcomes: R1–R4 verified
- Commands and artifacts: cargo test 13 OK; keygen/pubkey; cap-check; noise_dns PASS; live stats hs_ok=1/auth_ok=1
- Constraint and diff-scope check: туннель не тронут; ban нет; ключ только из файла; остаточный риск 32 слотов принят и задокументирован
- Final status: complete
