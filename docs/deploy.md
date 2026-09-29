# Деплой dmsg53 на n-de2 (без шума)

Реальные значения только в `/opt/srv/53/.env` и `/opt/srv/53/secrets/` (на сервере, не в Git).

## Подъём

```sh
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env up -d --build
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env ps
```

Ожидается: `slipstream` Up с маппингом `${DNS_BIND_IP}:53->5353/udp`, `msgd` `healthy`.

## Проверки (что гонять, что = PASS)

```sh
. /opt/srv/53/.env
DMSG_DOMAIN=$DMSG_DOMAIN DIAG_SERVER_PUB=<hex из keygen> timeout 180 python3 /opt/srv/53/diag/noise_dns.py
# ожидается: noise_dns=PASS, RESULT PASS
DMSG_DOMAIN=$DMSG_DOMAIN DIAG_SERVER_PUB=<hex> DIAG_TOKEN_FILE=/opt/srv/53/diag/.tok timeout 180 python3 /opt/srv/53/diag/enrol_dns.py
# токен — только файлом 600 (никогда env/argv/логи); ожидается: enrol_dns=PASS
DMSG_DOMAIN=$DMSG_DOMAIN DIAG_SERVER_PUB=<hex> DIAG_TOKEN_FILE=/opt/srv/53/diag/.tok timeout 180 python3 /opt/srv/53/diag/mbox_dns.py
# ожидается: mbox_dns=PASS (A→Б, без дублей)
dig +tcp @1.1.1.1 probe-p5.${DMSG_DOMAIN} A | grep status
# ожидается: не SERVFAIL (NXDOMAIN = authoritative отвечает через рекурсию)
```

`DIAG_SERVER_PUB` — вывод `msgd pubkey --key` (публичный, не секрет).
`s3_diag.py` в `diag/` — архив plaintext-эры, BROKEN (сервер ждёт Noise), не гонять.

## Операции через msgctl

```sh
S=/var/lib/msgd/msgctl.sock
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl stats       # счётчики
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl user-list    # пользователи (префиксы)
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl quotas       # квоты vs лимиты
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl invite-issue --out-file /var/lib/msgd/invite.txt  # URI только в файл 0600 (refuse-if-exists)
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl invite-revoke --file /path/token.hex
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl device-block --file /path/devkey.hex
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl backup        # снапшот db+blobs
```

Секреты — только файлами 600, hex в argv запрещён (светится в `ps`).
Без `--out-file` URI печатается в stdout + warning (только руками, не в скрипты/логи).

Ошибся блокировкой — `device-unblock --file <hex-файл>`. Потерял телефон — `device-block --file` + новый `invite-issue --out-file` (перепривязки старого ключа нет; unblock для этого не использовать).

## Backup / restore

- `backup` кладёт `snap-<ts>/` (msgd.db + blobs/) в volume, держит 3 штуки (ротация — до записи нового; недоснапшот удаляется), проверяет `integrity_check`.
- Покрывает: битую БД, ошибку оператора. НЕ покрывает: смерть диска (тот же диск, SPOF).
- Секреты в снапшот не входят (статичны): архивировать `secrets/` отдельно одной командой `cp -a`.
- Рестор (только руками, drill — на отдельной копии, никогда поверх прода):
  1. `compose stop msgd`
  2. удалить `msgd.db-wal/-shm` рядом с целью
  3. подменить `msgd.db` + дерево `blobs/` из снапшота
  4. `compose up -d`, проверить `dbversion` и `user-list`

## Ротация carrier-серта / noise-ключа (инвалидирует invites и пины!)

Строго по порядку, с проверкой после каждого шага; при сбое — откат к предыдущим файлам:

1. `backup` (точка отката).
2. Положить новые файлы в `secrets/` (noise_key: `0400`, owner `65532`; серт: `644`).
3. `compose up -d`, дождаться `healthy`.
4. Noise-DNS PASS.
5. Перевыпустить invites (`invite-issue`), старые недействительны; клиенты перепривязываются новым bootstrap.

## Диск, логи, перезапуск

- Следить за местом: `docker system df`, `du -sh` volumes. Капа числа снапшотов — 3 (код), за ростом `msgd-data` — глазами.
- Логи: `docker logs dmsg53-msgd-1` (в логах нет токенов/ключей — только факты).
- Перезапуск: `compose restart`; рестарт-цикл входит в smoke (данные в volumes).

## Факты P4–P5

- Mailbox A→Б через DNS PASS (~8.5s); enrol PASS; Noise PASS.
- Память idle: msgd доли MiB; volumes `msgd-data` + `blobs-data`, owner `65532` (иначе `SQLITE_CANTOPEN`).
- Серт диагностический (90 дней). Ключи: `noise_key` + `carrier_key.pem` закрыты от остальных.

## Пределы (честно)

- 1 endpoint, пилот ≤16, транспорт max-connections=32 (кап msgd не защищает слоты транспорта — остаток принят).
- Kill ≠ power-loss: тесты asserts порядка commit→ACK при WAL+FULL, не выживание при обесточивании.
- Звонки не обещаны. HA/федерации/групп нет.
