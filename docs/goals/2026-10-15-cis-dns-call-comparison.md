# Goal: сравнение доступных в СНГ DNS для звонков

Status: complete
Source: пользователь 2026-10-15: исключить Cloudflare/Google, использовать работающие в СНГ DNS; одобрил сравнение текущего Яндекса и DNS операторов на одной сборке, короткий звонок → 180 секунд при коротком PASS; «фиксируй новую цель и приступай к работе».
Last updated: 2026-10-15

## Objective
Проверить, устраняет ли смена рекурсивного DNS обрывы звонка Moto ↔ OnePlus на неизменном транспорте21ae19c: получить фактический DNS-путь и реальные результаты короткого/180-секундного звонка для Яндекса и доступных DNS активных сетей. Не объявлять DNS единственной причиной по одному успешному/неуспешному запуску.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: определить кандидатов и доказать фактически используемый DNS без скрытого fallback.
  - Source: одобренный план сравнения и ограничение «только ... работает в СНГ».
  - Acceptance: идентифицированы оба телефона/сети и установленные APK; найдены DNS активных сетей и Яндекс. Для каждого допускаемого сравнения известен действующий resolver/group, а не только сохранённый профиль. Cloudflare/Google не используются ни явно, ни резервно.
  - Primary evidence: ограниченный platform/network preflight, существующие app/native diagnostics и проверка владельца fallback.
  - Status: verified
  - Evidence: identities/hashes исходных APK подтверждены; operator/DHCP DNS извлечены из active LinkProperties обоих телефонов. Одна diagnostic APK9d4d41d0f3932dbbb478b6980bf1697c1802ad41474f698b9836294b0a5c409e/test063cb89f установлена-r обоим. Единственный source diff — fallback argument=profile.resolvers.clone(); обе попытки используют один выбранный numeric resolver. В receipts выбранные входы удерживаются, explicit_input_matches=true/cloud=false; platform input восстанавливается в finally. Девять DNS unit tests и ARM64/debug/test build PASS. Staged senderce53a261/config327c80e1 совпадают с21ae19c и padding39/128; vendor dirty inputs, normal source и normal native output сохранены/восстановлены.
- R2: сравнить реальный DNS-звонок на неизменной клиентской/carrier-связке.
  - Source: «На одной установленной сборке сравниваем ... Где короткий звонок проходит — запускаем3 минуты».
  - Acceptance: для доступной пары operator-DNS и Яндекса сохранены оба фактических framework-результата короткого звонка; только после парного короткого PASS выполняется тот же180s gate. Недоступность/отказ не подменяются PASS. Сменяются только DNS-входы, не транспорт, сеть, media policy или carrier.
  - Primary evidence: существующие CallClientGatesTest selected methods, свежий listener-ready, полные sanitized receipts обоих телефонов.
  - Status: verified
  - Evidence: принудительный Яндекс77.88.8.8 даёт оба short framework FAIL, Moto oldest987 age727431us/frontier986 (19encoded/2decoded/3RTP), OnePlus17/1/9. Поэтому Яндекс180s не запускался. DNS активных сетей дают оба short framework PASS: Moto285/284/287, OnePlus normalREMOTE_ENDED; обе стороны joined. Moto ACTIVE native_failed=false, но после LOCAL_ENDED/retirement native_failed=true без progress_failure — сохранённая метрика, не замаскированная; причина этого teardown marker отдельно не установлена. Operator180s выполнен после парного short PASS, оба framework FAIL примерно на100s media: Moto oldest21497 age729494us/frontier21496,19entries899B. Moto2505encoded/2346decoded/2488RTP, late142/PLC150; OnePlus2505/2421/2501, late80/PLC84; expiry0 оба. Финальные hardware clocks calibrated/healthy оба; оба ENDED/CONNECTION_LOST и joined. Ни один180s PASS не получен.
- R3: зафиксировать проверенный результат и следующий узкий шаг.
  - Source: «Если смена DNS помогает — закрепляем рабочий вариант. Если нет — разбираем задержку внутри Slipstream».
  - Acceptance: таблица resolver/path → short/180s outcome, без причинного вывода сверх измерения; при180s PASS записана точная проверенная конфигурация и способ её сохранения. Если смена не устраняет отказ, перечислены проверенные варианты и конкретный остающийся delivery failure, не «DNS ни при чём». Родительская R2 закрывается только её исходными физическими критериями.
  - Primary evidence: полные receipts, выбранные DNS-входы/фактическая группа, текущий goal checkpoint.
  - Status: verified
  - Evidence: таблица ниже и comparison.json. В проверенном A/B Яндекс не выдержал short, operator/DHCP DNS выдержали short, но не180s. Смена выбранного DNS не оказалась достаточным исправлением. Это не доказывает невиновность DNS, причину сообщённого инцидента, отсутствие transparent forwarding или место задержки внутри Slipstream. Рабочей180s конфигурации для закрепления не найдено. Следующий узкий этап родительской R2 — локализовать задержку конкретного покрывающего RTP/SRTCP delivery на operator-маршруте; не промотировать padding или менять600/720 по одному этому результату.

### Constraints
- Одна закреплённая transport21ae19c на обоих телефонах и carrierbaea123; backendb580/config/data/pins/topology и соседний интернет не менять. Незавершённый padding39/0 в vendor не собирать, не коммитить и не выкладывать в этой цели.
- Начать с текущего target APKba7d2e36 без пересборки. Доказанный blocker: gate видит только profile, Rust State не содержит active group, C отправляет unconnected UDP; socket table не сообщает sendmsg destination. Минимальная диагностическая изоляция: временно собрать existing start_with_fallback с profile.resolvers.clone() вместо Yandex, то есть обе последовательные попытки используют один выбранный список. Один и тот же diagnostic APK для всех кандидатов/обоих телефонов; transport21ae19c/sender/CC/media/carrier неизменны. Сохранить точный исходный diff/hash и восстановить исходник/обычные build outputs; это не production policy change, новый API/feature или release.
- Только MotoZY22JFJ5LP/cancunf и OnePlus2bcd17fa/OnePlus7Pro; install-r при доказанной необходимости, без Main/reset/Keystore/identity изменений. Сети/radio/volume/PIN не менять.
- Сохранять600/720/2880/80ms/640/JNI≤160, реальный capture/decode, hardware-calibrated/healthy assertions, mute/recreate/background/End/joined retirement. Framework OK1, ADB0, final−1 и неотрицательные status codes ending0 обязательны.
- Только DNS, фактически доступные в сетях телефонов; никакого Cloudflare1.1.1.1/Google8.8.8.8 и их вторичных адресов. Обычный DNS ответ/host test не равен Android Slipstream/call acceptance.
- Secrets/authority details — только owner-only `.local`/удалённые private files, не Git/логи/argv/env; clean allowlist tools. Никаких PCM/audio files, STT, новых сервисов/dependencies или постоянной телеметрии.

### Non-goals
- Новые transport/CC/repair/padding/feedback изменения, увеличение таймаутов, новая сеть/VPN/proxy, тестирование причины сообщённого инцидента Яндекса, широкая матрица публичных DNS, повторная упаковка release APK.

## Change Envelope
- Эта цель и ссылка/current checkpoint в `2026-10-11-call-client-apk.md`.
- Существующие isolated device gates и private orchestration/fixtures в `.local/call-client-apk/` и новой `.local/cis-dns-call-comparison/`; полные sanitized receipts в gitignored target/.
- Разрешено наблюдение текущих network/DNS и выбранные app-scoped DNS входы с восстановлением. Подтверждённый blocker R1 допускает только временную однострочную сборочную замену аргумента fallback в crates/core/src/dns.rs; все остальные runtime/public API/native transport/persistent schema неизменны. Точная baseline APK и native bytes сохраняются, install-r без reset.
- Незавершённые шесть vendor-файлов и unrelated design PNG сохранить нетронутыми.

## Results
| Выбранный DNS на обоих клиентах | Short10s | 180s | Наблюдение |
|---|---|---|---|
| Яндекс77.88.8.8 | оба FAIL | не запускался по контракту | Moto ledger727431us, peer feedback frontier не покрывает oldest987 |
| DNS своей активной сети: Moto LTE DNS#1, OnePlus Wi-Fi stub DNS#1 | оба PASS | оба FAIL, ~100s media | Moto ledger729494us, oldest21497/frontier21496; оба final hardware healthy/calibrated |

Это сравнение двух выбранных UDP destinations, а не измерение всей рекурсивной цепочки или статистическая оценка провайдера. В сохранённых input checks configured_count1/explicit_input_matchestrue/cloudfalse; backup использует тот же список конструкцией diagnostic build. Точные network DNS адреса находятся в owner-only fixtures, не в public profile/authority.

## Current Checkpoint
- Closes: R1–R3, завершены.
- Smallest next action: нет в этой конечной цели. Родительская R2 остаётся открытой; следующий delivery-discriminator отдельно, без автоматического возврата к paused padding integration.
- Evidence: полные шесть call receipts, comparison.json, source/native/artifact provenance и verified restoration. Яндекс180s не запускался из-за short FAIL — предусмотренная ветвь, не waiver.

## Current State
- Resolved: R1–R3. Диагностический native SHA b2a71d433337c3e0ce2dbd015b9a3f63a777b241125c5430edc6549ebbd3a36b; обычный native f332c701 и dns.rs восстановлены после freeze. После опытов точный обычный targetba7d2e36 восстановлен-r обоим; Moto test063cb89f/OnePlus test826fb6 восстановлены к исходным значениям. Generated debug APK тоже возвращён к baseline. Ни reset, ни смены identity/Keystore/сети/volume/PIN; carrierbaea123/backendb580 неизменны. Незавершённые шесть vendor inputs сохранены, не попали в build и не промотированы.
- Last relevant evidence: `target/cis-dns-call-comparison/{baseline,artifacts,comparison,moto-install,oneplus-install,moto-restore,oneplus-restore}.json`, isolation.patch; полные `*-{yandex10,operator10,operator180}.json`. Operator180 Moto16219B/SHA c71616581fcbf091ff8c855d258ef7a165535a38f0ef04c6bb0b57d4b466a6a6, OnePlus55144B/SHA21779b2ac128cab08a4540987a48b43264558aef622bfbe2f36a7b195a54463d. Post-restore encrypted TEXT на обычном APK: оба actual framework PASS; sender unchanged client05 проверяет exact new mid DELIVERED, receiver fixed-body match не является unique-message freshness proof.
- Blocker: нет для завершения этого сравнения. Причина полного delivery stall не локализована; healthy final clocks и последний control processing154us не дают общей временной шкалы/своевременного covering report.
- Next: none in this goal; parent R2 remains in_progress.

## Material Decisions
- 2026-10-15: пользователь сменил ближайшую задачу с padding на контролируемое DNS-сравнение. Сообщённая деградация Яндекса — гипотеза, не установленная причина. Завершившийся background padding integration не промотируется.
- 2026-10-15: вместо нового runtime selector/telemetry API или ненадёжного вывода по profile/Ready используется существующий supervisor с идентичным выбранным resolver в primary/backup. Только диагностический build; оригинальная one-line source/configuration восстановлена после freeze артефакта. Сборщик экспортирует vendor Git objects21ae19c, не незакоммиченный padding overlay.
- 2026-10-15: первый artifact collector ошибочно искал set_default_padding вместо фактического set_padding_policy; точное сравнение config bytes с pinned Git object и sender hash прошло. Большой APK transfer exec-raw превысил180s, SFTP успешно завершился; это failures инструмента/проверочного скрипта, не app/DNS evidence. Сети, громкость и аккаунты не менялись.

## Completion
- Resolved outcomes: R1–R3 verified; проверка гипотезы завершена с отрицательным sustained результатом, не «звонки исправлены».
- Commands and artifacts: `cargo test -p dmsg-core --lib dns::tests`9PASS; default `android/build-native.sh`; `./gradlew -PgateInstall=true assembleDebug assembleDebugAndroidTest`; unchanged selected client02/client03 media gates, затем client05/client06 TEXT после восстановления. Все actual framework results и hashes сохранены в target/cis-dns-call-comparison/.
- Constraint and diff-scope check: source override ровно одна строка и полностью восстановлен; sender/config/PIN/CC/media/crypto неизменны, unknown provenance не подменён Ready/profile assertion; no Cloudflare/Google. Normal APK/test/native возвращены, pending vendor work не затронут. Не получено исходной long-call приёмки родительской цели.
- Final status: complete (DNS comparison only).
