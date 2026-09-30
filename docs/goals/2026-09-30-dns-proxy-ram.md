# Goal: убрать UDP docker-proxy из внешнего DNS-пути dmsg53

Status: active
Source: инструкция пользователя «Переходи к реализации плана, жду результат, потом коммит и пояснение»; исправленный план аудита RAM-инцидента.
Last updated: 2026-09-30

## Objective
Внешний DNS-трафик dmsg53 проходит через kernel DNAT, не создавая гигабайтный UDP working set docker-proxy; существующий клиент и persistent identity продолжают работать.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract
- R1: стабильный bridge IP + точечный persistent DNAT/forward.
  - Source: исправленный план, серверное исправление.
  - Acceptance: external public-IP UDP53 → static-private-IP UDP5353; source resolver сохранён, ответ public-IP:53; совместное пересоздание сохраняет маршрут.
  - Primary evidence: Compose config, nft syntax/counters, packet aggregates, joint recreate.
  - Status: verified
  - Evidence: baseline 14:05+02: proxy RSS 1,553,900 KiB, RAM available 762/3916 MiB; bridge dynamic, DNAT отсутствует. Joint down/up без -v применил static-IP вне dynamic pool. DNAT/forward counters растут; bridge capture 2593/2593 resolver sources preserved, 2593 replies. External replies public-IP:53. Existing nftables reload PASS: JSON ruleset идентичен, кроме counters/handles; service enabled.
- R2: RAM/FD стабильны с неизменённым Android-клиентом.
  - Source: исправленный план, приёмка RAM.
  - Acceptance: 5 min empty FGS, 5 min fixed messaging load, ≥180 s quiescence, repeat load; bounded proxy sockets/FD/RSS, available RAM >20%.
  - Primary evidence: timed host memory/proxy/socket/query samples.
  - Status: verified
  - Evidence: 300 s empty FGS (17 successful empty polls, тот же PID); два 300 s load runs по 5 bidirectional pairs/10 messages, все dedup/cursor checks PASS; между ними 190 s quiet, authoritative Q/R=0. Все timed samples: proxy RSS6492 KiB, FD7, socket1; available73.00–74.77%, без накопления на repeat. Conntrack first load max20967, quiet→16, repeat max20026.
- R3: recursive DNS/Noise/E2E и restart без потери identity/дублей.
  - Source: исправленный план, функциональная приёмка.
  - Acceptance: moto↔Linux native DNS peer в обе стороны; repeat fetch без дублей/cursor regression; interruption/retry сохраняет ciphertext; серверные volumes/keys/pins сохраняются.
  - Primary evidence: manual selected device gates + native DNS peer + recreate checks.
  - Status: verified
  - Evidence: 11 bidirectional pairs PASS; финальный bridge-only forward тоже проверен capture: 6648/6648 resolver sources preserved и столько же replies. После server stop, offline queue, actual Android force-stop и joint force-recreate fresh-process retry gate PASS: прежние account/inbox/ciphertext SHA256, queued→accepted; native DNS fetch1→0/skip counters0. Same-resolver restart identity gate PASS. Post-recreate образы, static IP, shared namespace, UID65532, ro secrets, volumes и loopback7000 прежние; persistent reload вновь PASS.
- R4: проверенный результат, коммит и пояснение.
  - Source: текущая инструкция пользователя.
  - Acceptance: intended deployment/docs diff проверен и закоммичен; итог содержит реально выполненные проверки.
  - Primary evidence: git diff/status/log и итоговый ответ.
  - Status: in_progress
  - Evidence:

## Constraints / non-goals
- Bridge/shared namespace/non-root5353/backend127.0.0.1:7000 сохраняются. Секреты только ro-files, данные в прежних volumes.
- Не менять глобальный Docker, соседний туннель, QUIC/DNS semantics, Android polling/APK или существующую identity/Keystore.
- Не очищать conntrack глобально; rollback только endpoint rules. Не требовать reboot хоста.
- Linux peer не заменяет два физических Android; optional client/NXDOMAIN work вне задачи.
- Реальные адреса/домены, bearer/keys, raw QNAME не в Git/логи. `53-opendesign/` — чужая работа.

## Change Envelope
- `deploy/compose.yml`, `.env.example`, узкий nft example; `docs/deploy.md`, этот goal.
- Remote: host-local Compose/env/firewall fragment + включение в существующий nftables.conf; joint recreate без удаления volumes, короткие ignored diagnostic fixtures.
- Не менять runtime код приложения/сервера/transport и зависимости.

## Current Checkpoint
- Closes: R4.
- Next: commit проверенного intended diff и запись closure.
- Replan if: failed recursive delivery или proxy sockets растут; использовать endpoint rollback, не расширять firewall.

## Material Decisions
- Proxy process/published port остаются для редких host-local OUTPUT probes; приёмка — внешний hot path без proxy.
- Persistent rules включаются в существующий nftables.conf; runtime установка не flush-ит firewall.
- Manual instrumentation завершает target process; поэтому steady FGS стартует через обычную UI-кнопку после gates. Окна без работающего FGS не используются как empty-poll evidence.
- Forward дополнительно требует выход в `br-*`: новый DNAT flow при отсутствии bridge не разрешается через внешний default route. Для offline gate используется `compose stop`, сохраняющий project network.

## Checkpoint History
- 14:13+02: R1 deployed с backup `snap-1790770298` и host-local rollback configs. Образы неизменны, оба UID65532, backend loopback7000, secret mounts ro, private keys400 owner65532; schema4/прежние volumes сохранены.
- 14:19+02: первый moto↔native recursive DNS E2E pair PASS; Android durable inbox +1, repeat fetch0/monotonic cursor; native fetch1→0, skip counters0.
- Host `cargo build -p msgd && cargo test --workspace`: 146 PASS, существующий conditional native loopback test ignored; runtime DNS gate проверяется отдельно.
- R2 timed windows (server +02): empty 14:24:41–14:29:41; first load probe 14:31:13–14:36:23; quiet 14:37:15–14:40:25; repeat load probe 14:40:54–14:46:04. External query totals: 49725, 116767, 0, 110118 соответственно. Load cadence: одна пара/60 s; existing manual gates, FGS UI restart между парами. APK/poll interval неизменны.
- 14:48–14:50+02: R3 interrupted retry/recreate/identity gates PASS. `up --no-build --force-recreate --wait` обоих сервисов сохранил volumes/keys; final nft syntax + persistent reload + unchanged images/isolation checks PASS. Working FGS восстановлен через UI и оставлен в background.
- 14:52:43–14:54:43+02: post-recreate background FGS verification PASS: proxy RSS6424 KiB/FD7/socket1, available73.60–74.72%, external Q/R16764/16764; bridge capture1680/1680 sources/replies. APK lastUpdateTime прежний; FGS работает в background.

## Completion
- R1–R3 verified; осталось зафиксировать проверенный diff в Git и закрыть R4.
