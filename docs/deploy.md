# Деплой dmsg53 на n-de2 (без шума)

Реальные значения только в `/opt/srv/53/.env` и `/opt/srv/53/secrets/` (на сервере, не в Git).

## Account-auth: целевой сценарий и текущая реализация

Пользовательский вход определён в `ARCHITECTURE.md` §6: один публичный код/QR подключения → логин/пароль → диалоги. Код несёт server domain/full DER/Noise pubkey, **не** общий access key или приглашение. Создание аккаунта регулируют два серверных режима: `open` / `invite_only` (default); переключение не запрещает вход существующих пользователей. Invite нужен отдельно только при signup в `invite_only`.

Выдача публичного профиля, password credentials, переключатель policy и password-authorized device replacement **ещё не реализованы**. Команды для них появятся вместе с кодом/тестами по R12–R16 в `docs/goals/2026-09-29-client-track.md`; ниже перечислены только реально существующие диагностические/административные команды. `invite-issue` сейчас выдаёт секретный legacy bootstrap, не новый публичный server-код и не рекомендуемый экран входа.

## Подъём

```sh
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env up -d --build
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env ps
```

Ожидается: `slipstream` Up с маппингом `${DNS_BIND_IP}:53->5353/udp`, `msgd` `healthy`.

## DNS без UDP docker-proxy на внешнем hot path

На этом хосте Docker работает без iptables. Published UDP port сам по себе
обслуживается userspace proxy: каждый новый peer IP/port держит socket и большой
buffer до timeout. Внешний DNS направляем через **точечный kernel DNAT**:

```text
resolver → public-IP:53 → nft DNAT → bridge static-IP:5353 → 127.0.0.1:7000
host-local OUTPUT probe → retained published port / docker-proxy
```

### Адреса и установка

- В host-local `.env` задать `DMSG_NETWORK_SUBNET`, `DMSG_NETWORK_DYNAMIC_RANGE`,
  `DMSG_SLIPSTREAM_IPV4`. Сверить Docker networks и host/VPN routes: без overlap.
- Static IP принадлежит subnet, **не dynamic pool**, не gateway/network/broadcast.
  Выделенный project network нельзя использовать для посторонних static endpoints.
- Сохранить прежние Compose/env/firewall и сделать `msgctl backup`. При добавлении
  IPAM выполнить `compose down` и `up -d --no-build` **без `-v`**, оба сервиса вместе.
  Использовать команды выше с абсолютными compose/env paths; ключи не менять.
- Скопировать `deploy/dns-forward.nft.example` в `/etc/dmsg53-dns.nft`; подставить
  public IP/port и static private IP/internal port из deployment env, вне Git.
- Fragment рассчитан на существующие `inet firewall forward` и `ip nat` этого
  хоста. Forward accept стоит до policy drop; invalid/established rules сохраняются.
- Forward разрешён только в Docker bridge `br-*`: при удалении project network
  новые DNAT flows не должны уйти через внешний default route.
- В **конец** существующего `/etc/nftables.conf` добавить
  `include "/etc/dmsg53-dns.nft"`. Существующий nftables service должен быть enabled.

```sh
nft -c -f /etc/nftables.conf       # полный persistent config, только syntax check
nft -c -f /etc/dmsg53-dns.nft
nft -f /etc/dmsg53-dns.nft         # первый runtime install, без flush ruleset
nft list chain ip nat dmsg53_dns
nft -a list chain inet firewall forward
```

Не применять fragment повторно к live ruleset: rules добавятся ещё раз.
Для reload использовать существующий `systemctl reload nftables`, который
загружает весь сохранённый config. Не менять Docker daemon, не добавлять blanket
SNAT/forward allow: входной resolver source должен сохраниться, ответы — public IP:53.
IP forwarding и существующий masquerade должны уже работать.

### Приёмка и rollback

- DNAT/forward counters растут на **новых flows**; established packets могут
  пройти ранее существующий accept. Published port/proxy остаются — это не ошибка.
- Проверить recursive Android↔native-peer Noise/E2E, dedup/cursor, joint recreate;
  backend только loopback7000, прежние volumes, ro secrets и non-root UID.
- Измерять **host** available RAM, proxy RSS/FD/sockets и conntrack: 5 min empty FGS,
  5 min fixed message workload, ≥180 s quiescence, repeat workload. RAM >20%,
  proxy working set bounded; container stats недостаточны. RSS не обязан сразу упасть.
- Rollback: удалить include из persistent config; по `nft -a` удалить только
  forward-rule с comment `dmsg53 DNS forward only`, затем flush/delete **только**
  chain `ip nat dmsg53_dns`. Published port восстановит прежний внешний путь.
- Старые UDP conntrack tuples переживают смену правил. Дождаться relevant timeouts
  (на проверенном хосте UDP30/stream120 s), либо удалить только affected endpoint
  entries. **Не flush conntrack глобально.** Proxy timeout90 s — другое ограничение.
- Для возврата прежней IPAM: удалить endpoint rules, восстановить Compose/env,
  совместно `down`/`up` без `-v`. Не восстанавливать DB поверх живого production.

## Текущие wire/transport проверки (не приёмка нового login/register)

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
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl invite-rebind --file /path/old-device-public.hex --out-file /var/lib/msgd/rebind.txt 86400
docker exec -e MSGCTL_SOCK=$S dmsg53-msgd-1 /usr/local/bin/msgd msgctl backup        # снапшот db+blobs
```

Секреты — только файлами 600, hex в argv запрещён (светится в `ps`).
У обычного `invite-issue` без `--out-file` URI печатается в stdout + warning (только руками, не в скрипты/логи); `invite-rebind` требует файл и никогда не печатает URI.

Ошибся блокировкой — `device-unblock --file <hex-файл>`: разрешено только без другого активного устройства этого аккаунта. После перепривязки старые login-токены остаются отозванными; unblock не восстанавливает их. `device-block` + обычный `invite-issue` создаёт **другой аккаунт**, не перепривязку старого.

### Существующая административная перепривязка (R11)

Это служебная операция с revocation старого доступа, не основной пользовательский вход. Целевой login на новом устройстве: проверить логин/пароль → явное подтверждение замены → прежний account/contact ID, свежие device/E2E keys, peer warning, без старой истории. Такой UI/auth-path ещё предстоит реализовать; следующие команды уже существуют и сохраняются в runbook для администратора.

```sh
MSGCTL_SOCK=/var/lib/msgd/msgctl.sock msgd msgctl invite-rebind \
  --file /path/old-device-public.hex --out-file /var/lib/msgd/rebind.txt [ttl_secs]
```

- `--file`: **публичный Noise static старого устройства**, 32 байта в виде 64 hex-символов (верхний/нижний регистр, whitespace/newline по краям допустим). Не приватный ключ, не Noise-ключ сервера, не Olm identity, не contact ID. Файл берётся из известной администратору записи потерянного тестового устройства; не подставлять ключ рабочего аккаунта. Пути внутри `docker exec` должны существовать в контейнере.
- `--out-file` обязателен: самодостаточный `dmsg://join/...` с token256 пишется только в новый файл `0600`, в stdout — лишь `ok`, URI/token/ключи не логируются. CLI резервирует файл через `create_new` **до** запроса: существующий путь/симлинк или отсутствующий parent → отказ без изменения БД. TTL по умолчанию 86400 с; целое ≤0 нормализуется в 1 с, переполнение срока отвергается.
- Одна транзакция отзываёт старое устройство, все прежние bound login-invites аккаунта и его pending rebind-invites, удаляет prekeys только старого устройства и выпускает новое приглашение с target user_id. Уже блокированный старый ключ тоже подходит. Если другой ключ аккаунта уже активен, операция отвергается без изменений. После commit закрываются old-key live/pre-enrol streams.
- Новый клиент создаёт свежие Noise/Olm ключи и проходит **существующий ENROL wire**. Claim сохраняет `user_id` и `contact_id`, создаёт ровно одно активное устройство, привязывает token и отзывает остальные unbound rebind-токены аккаунта в одной транзакции. Старые ключи/identity/prekeys не копируются. Контакты должны заново подтвердить сменившуюся E2E identity по QR/SAS (клиентский warning gate проверяется отдельно).
- Повтор ENROL тем же новым Noise static возвращает тот же аккаунт, даже после TTL; другой Noise key не может забрать bound token. Revocation всегда запрещает replay. При повторном выпуске до claim действует только последний rebind-invite; повторный выпуск по старому ключу после успешного claim отвергается, пока новый ключ активен.
- **История не восстанавливается.** При claim новый cursor устанавливается на максимальный `seq` этого получателя на момент claim: ни старые delivered, ни pending ciphertext (включая пришедшие между выпуском и claim) не выдаются новому устройству. Никакого DELIVERY_ACK за потерянный клиент не подделывается: delivered-флаги, old-device cursors и другие пользователи не меняются. Строки ciphertext сохраняются для sender/message_id дедупликации, квот и обычного TTL/GC; mailbox/blob квоты остаются прежними на user_id, не обнуляются. Blobs, contact permissions и чужие mailbox данные не переносятся/не удаляются. Старый ciphertext свежими Olm-ключами не расшифровать; даже сообщение после claim, отправленное peer со старой Olm-сессией, требует клиентского identity-change workflow.
- Ошибки CLI — статические категории: usage/exit2; `invalid-source`, `output-unavailable`, `rejected`, `transport-failed`, `output-failed`/exit1. Сырой Unix socket отвечает `err` без SQL/credentials. Wire ENROL: неизвестный/битый token→BAD1; revoked invite→REVOKED3 **до TTL**; same-bound-key→проверка device revocation/сохранённого аккаунта, TTL игнорируется; остальное expired→EXPIRED2, затем occupied/known key→BOUND_OTHER4; SQLITE_BUSY→BUSY7, store failure→BAD1. Неизвестный/битый target fail-closed; неудачная транзакция не оставляет device/cursor/bind.
- Если ответ потерян или запись/sync файла не удалась **после серверного commit**, старое устройство уже отозвано: автоматического unblock нет. CLI удаляет неготовый output, при write/sync failure best-effort отзывает полученный token; transport failure может оставить недоступный pending invite. Повторить по тому же old-public файлу с **новым** output path: новый выпуск атомарно инвалидирует прежний pending invite. Успехом считать только exit0 + `ok` + файл, а не отсутствие URI в stdout.

### Обновление schema 3 → 4

До обновления — согласованный `msgctl backup` и отдельное хранение server secrets. Schema4 добавляет nullable `invites.rebind_user_id` (обычные invites остаются NULL) и partial UNIQUE index `one_active_device_per_user` по `devices(user_id) WHERE revoked=0 AND user_id IS NOT NULL`. Миграция транзакционная и идемпотентна; если в старой БД уже два active device одного user_id, старт fail-closed без выбора победителя/частичной миграции. После обновления проверить `dbversion` = `4`, healthy/pong и штатный Noise-DNS smoke; rebind/DNS/peer-warning gate проводить только на disposable account (например, отдельный `.gate`), рабочую identity не отзывать. Downgrade схемы не обещается; restore drill — отдельная копия, не поверх production.

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
5. Для действующих legacy клиентов перевыпустить secret bootstrap через `invite-issue`; это диагностика/совместимость, не публичный код подключения. В новом account-auth потребуется отдельно перевыпустить доверенный public profile и проверить сохранность аккаунта после обновления pins; эту операцию не объявлять существующей до реализации. Смена server pins сама по себе не восстанавливает историю или не разрешает silent device replacement.

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
