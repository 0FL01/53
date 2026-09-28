# Деплой dmsg53 на n-de2 (без шума)

Реальные значения только в `/opt/srv/53/.env` и `/opt/srv/53/secrets/` (на сервере, не в Git).

## Подъём

```sh
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env up -d --build
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env ps
```

Ожидается: `slipstream` Up с маппингом `${DNS_BIND_IP}:53->5353/udp`, `msgd` `healthy`.

## Проверки

```sh
. /opt/srv/53/.env
DMSG_DOMAIN=$DMSG_DOMAIN timeout 120 python3 /opt/srv/53/diag/s3_diag.py
# ожидается: clients_ok=2/2, RESULT PASS
dig +tcp @1.1.1.1 probe-s3.${DMSG_DOMAIN} A | grep status
# ожидается: не SERVFAIL (NXDOMAIN = authoritative отвечает через рекурсию)
```

## Факты S3 (2026-09-28)

- Диагностика 2/2 PASS до и после `compose restart`, ~1.2–1.3s.
- Память: slipstream ~1.6 MiB, msgd ~0.5 MiB (idle).
- Серт диагностический (90 дней). Перед первыми invites: выпустить боевой, обновить `secrets/`, пересобрать пин у клиентов.

## Пределы (честно)

- 1 endpoint, пилот ≤16 устройств, транспорт max-connections=32.
- Один сервер = SPOF; backup/restore — M7, пока только volume `msgd-data`.
- Звонки не обещаны (M1-V вне этого деплоя).
