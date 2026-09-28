# Baseline M0 — транспорт на пине (агрегаты, без секретов)

Дата: 2026-09-28. Rev: `d7cd555` (`feat/rust-parity-ab`).
Вложенные сабмодули: picoquic `2ee2ecd`, SPCDNS `d291537`, lua-resty-base-encoding `bd9246f`, quick_arg_parser `153b0ac`.

## Сборка и гейты (локально, Fedora)

- Конфиг: `meson setup` (shared; static-линк недоступен — нет `libstdc++-static`), OpenSSL headers из miniconda 3.0.17, рантайм `LD_PRELOAD=/usr/lib64/libstdc++.so.6` (conda libstdc++ старше системного).
- `meson test --print-errorlogs`: **4/4 OK** — `protocol`, `path-guard`, `pin`, `runtime` (runtime ~34s).
- Sanitizer: **отложен** — нет `/usr/lib64/libasan.so.8`, `/usr/lib64/libubsan.so.1`; прогон переносится в bookworm-образ на n-de2 (там тулчейн из Dockerfile).

## DNS-путь (наблюдения, не приёмка)

- TCP работает: публичный резолвер отвечает по TCP; NS-гlue поддомена резолвится рекурсивно по TCP.
- Прямой запрос к туннельному домену даёт SERVFAIL — **ожидаемо до деплоя**: authoritative сервер ещё не поднят.
- Один из публичных резолверов по TCP из этой точки таймаутит — довод за TCP-first с выбором резолвера по факту; финальный выбор фиксируется на S3.
- Рекурсивный путь к поддомену полностью закроется после деплоя S3 (прямой authoritative ≠ зачёт).

## Вывод

Пин собирается из чистого checkout, штатные сьюты зелёные. Полный M0-зачёт (sanitizer + рекурсивный путь) — после S1–S3 на n-de2.
