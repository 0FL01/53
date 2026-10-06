# Деплой 53 на n-de2

Реальные значения только в `/opt/srv/53/.env` и `/opt/srv/53/secrets/` (на сервере, не в Git).

## Единая account-auth логика и fresh dev rollout

Реализованы public `dmsg://server/` profile, wire2/server schema5/core schema6, password signup/login, key-only resume, persisted `open` / `invite_only` (default) и confirmed device replacement. Приглашение отдельно требуется только для signup в invite_only. ENROL/token-replay/credential attach/admin invite-rebind удалены. Команды ниже относятся к новой ревизии; wire — `docs/protocol.md`, CLI/details — `crates/server/README.md`, Android gates — `android/AUTH_GATES.md`.

**2026-10-01 разрешённый fresh dev rollout выполнен:** remote healthy/schema5, policy `invite_only`; actual recursive Android↔native signup/resume/E2E/retry/trust gates green. Старые16 dev users/devices потеряны намеренно после backup. Основной Android package/Keystore не стирали/не обновляли: physical acceptance только `.gate`.

Позднее main rollout R22 выполнен после отдельного явного разрешения dev reset:
branding `install -r` сохранил несовместимую local schema0 и показал Store. Scoped
main `pm clear` → fresh6/новая identity → actual UI signup/reopen/recursive messages
и persistent Delivered verified, один main53 оставлен рабочим. Не менять PRAGMA и
не добавлять migration/automatic wipe ради несовместимого dev state. Проверяемый
installer — `android/dev-install.py`, main opt-in gates — `android/AUTH_GATES.md`;
server schema5/pins/tunnel при этой коррекции не менялись.

- Backup `snap-1790845865`: schema4, DB118784 bytes, blobs0, integrity ok; independent protected DB/source/Compose/env/secrets archive и rollback image `dmsg53-msgd:pre-r21-schema4-1790845865` вне wipe targets.
- Source export `7f3fcaa0136a4dbfb6af904c3d9c0f969d50a5f0`, locked Docker workspace fix `a6d21e3`; new msgd image `sha256:e69dfe5b59b7ef7d5510076a5ed35f8ed4f4f686adde08b525e666e226c28ec4`, promoted `dmsg53-msgd:s1`.
- Carrier reused unchanged: `sha256:bb6243409d1909f3250e289052ade0b2f6d5912961c3d2c87f52afdef0152e73`. Joint recreate 09:18:15–16 UTC; secret bytes/ro mounts, pins, env/topology/static IP/volumes/nft/original tunnel unchanged.
- Only verified project dmsg53 DB/WAL/SHM and already-empty blob volume cleared; not `down -v`, global prune/firewall reset or automatic migration. Private exact records — `.local/frontend-rollout/`; no domains/IPs/secrets in this runbook.

Новый binary отклоняет старую БД read-only без migration/wipe. Для нового несовместимого reset нужны явное разрешение, backup и scoped targets; текущее разрешение не является общим production-wipe регламентом. Schema4 snapshot требует retained old image, не schema5 binary.

## Подъём / обновление совместимой schema5

```sh
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env up -d --build
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env ps
```

Ожидается: `slipstream` Up с маппингом `${DNS_BIND_IP}:53->5353/udp`, backend `53-1` `healthy`, `53ctl dbversion` = `5`. Не менять endpoint/topology/ключи исходного туннеля.

### Совместимый rollout 2026-10-06: 53 / 53ctl / 53-1

Image `53:s1`, entrypoint `/usr/local/bin/53`, direct control command `53ctl`;
backend container `53-1`. Внутренние Compose project `dmsg53`, service `msgd`,
data/blob volume names, data paths и Unix socket оставлены прежними: их смена
могла бы незаметно подключить пустую БД. Обновить только backend можно так:

```sh
docker compose -f /opt/srv/53/deploy/compose.yml --env-file /opt/srv/53/.env \
  up -d --no-deps --build msgd
docker exec 53-1 53ctl ping
docker exec 53-1 53ctl dbversion
docker exec 53-1 53ctl registration-mode
```

Проверено: до/после recreate одинаковые schema5, digests всех таблиц, volume
identities и public server-profile hash; carrier container ID/PID не менялись.
`pong`, `5`, `invite_only`, затем real recursive Android signup/key-resume PASS.
Никаких wipe/migration/pin rotation. Terminal QR PTY issue/re-render проверен;
non-TTY `qr-invite` exit2 без нового приглашения.

Deployment APK включает этот же доверенный **публичный** профиль при сборке:
`-PserverProfileFile=/absolute/path/to/public-profile.txt` или
`sh deploy/build-apk.sh /absolute/path/to/public-profile.txt`. Нет credentials
или приглашения в asset. Без входного файла generic APK сохраняет manual trust
preview. Main update без reset сохранил identity/history/wrapped Keystore key;
installed APK/native/asset и actual DNS key resume проверены. File onboarding
и ограничения optical/SAF evidence — `android/AUTH_GATES.md`; durable goal —
`docs/goals/2026-10-06-invite-onboarding.md`.

## DNS без UDP docker-proxy на внешнем hot path

На этом хосте Docker работает без iptables. Published UDP port сам по себе
обслуживается userspace proxy: каждый новый peer IP/port держит socket и большой
buffer до timeout. Внешний DNS направляем через **точечный kernel DNAT**:

```text
resolver → public-IP:53 → nft DNAT → bridge static-IP:5353 → msgd private-IP:7000
host-local OUTPUT probe → retained published port / docker-proxy
```

### Адреса и установка

- В host-local `.env` задать `DMSG_NETWORK_SUBNET`, `DMSG_NETWORK_DYNAMIC_RANGE`,
  `DMSG_SLIPSTREAM_IPV4`. Сверить Docker networks и host/VPN routes: без overlap.
- Static IP принадлежит subnet, **не dynamic pool**, не gateway/network/broadcast.
  Выделенный project network нельзя использовать для посторонних static endpoints.
- Сохранить прежние Compose/env/firewall и сделать `53ctl backup`. При добавлении
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
- Проверить recursive Android↔native-peer Noise/E2E, dedup/cursor, независимые
  рестарты carrier/msgd; backend только закрытый private IP:7000, прежние
  volumes, ro secrets и non-root UID.
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

## Новый wire/transport smoke после rollout

`cargo build -p msgd --examples`: `noise_diag` для AUTH_DOMAIN/WELCOME, `auth_diag` для signup/login/resume, `mbox_dns` для свежих signup двух disposable devices и SEND/FETCH/ACK. Они подключаются к **локальному pinned slipstream-client endpoint**, не публичному backend TCP. `DIAG_PORT`, `DIAG_DOMAIN`, `DIAG_SERVER_PUB` — public metadata. Auth: `DIAG_DEVICE_KEY_FILE`, `DIAG_PAYLOAD_FILE`, операция `DIAG_OPERATION=signup|login|resume`; mailbox: `DIAG_SIGNUP_A_FILE` / `DIAG_SIGNUP_B_FILE`. Это пути к bounded owner-only secret files0600, payload через shared `auth::build_signup/build_login`, не credentials в env. Результат должен быть PASS, не timeout/пропущенная fixture.

Старые host-local `enrol_dns.py`, token-based `mbox_dns.py` и plaintext `s3_diag.py` не являются рабочими smoke-командами wire2. Recursive Android signup/resume/E2E теперь проверены: production resolvers без override, actual C peer, received1 then0 обе стороны/all skipped0, exact ciphertext retry/status/history/replacement. ADB/SSH не переносили messages; evidence — `android/AUTH_GATES.md` и gate checklist.

## Операции через 53ctl

```sh
docker exec 53-1 53ctl stats
docker exec 53-1 53ctl dbversion
docker exec 53-1 53ctl server-code  # public profile
docker exec 53-1 53ctl registration-mode invite_only  # или open; без аргумента читает
docker exec -it 53-1 53ctl qr-invite  # secret terminal QR + private file, TTL 24 h
docker exec -it 53-1 53ctl qr-invite --ttl 3600
docker exec -it 53-1 53ctl qr-invite --file /var/lib/msgd/invites/example.invite
docker exec 53-1 53ctl invite-issue --out-file /var/lib/msgd/invite.txt
docker exec 53-1 53ctl invite-revoke --file /var/lib/msgd/invite.txt
docker exec 53-1 53ctl device-block --file /path/device-public.hex
docker exec 53-1 53ctl user-list
docker exec 53-1 53ctl quotas
docker exec 53-1 53ctl backup
```

Секреты только bounded regular owner-only файлами0600, значения не в argv/env/logs. `invite-issue` требует **новый** `--out-file` (refuse-if-exists/symlink, parent должен существовать): standalone base64url43 signup invitation, stdout только `ok`. TTL default86400; expiry/revocation/one-time consumption enforced server-side. При write/sync failure partial output удаляется, выданный invite best-effort отзывается. Списки redacted, password hashes не выводятся.

`device-unblock --file <public-device-key-hex-file>` исправляет временный block только ещё не retired устройства, без другого active device. Retired после replacement key нельзя оживить unblock. Приглашение не заменяет LOGIN и не перевыпускает аккаунт.

### Password-confirmed replacement

Новый key получает challenge после проверки credentials, **без мутации**. Пользовательский confirm и второй LOGIN с expected-old-key атомарно сохраняют account/contact IDs, отзывают прежний доступ/закрывают sessions, создают fresh device с cursor на high-water mark. Потеря ответа retry-safe, concurrent challenge требует нового подтверждения. Старый ciphertext не выдаётся новому аппарату и не считается доставленным, quota/dedup/TTL сохраняются. Peer binding Noise/Ed/Curve меняется явно: STOP до confirm, pending новое сообщение не ACK/discard. Пароль не восстанавливает историю; admin invite-rebind/password-bypass отсутствует. Проверять только disposable account, не рабочую identity.

## Backup / restore

- `backup` кладёт `snap-<ts>/` (msgd.db + blobs/) в volume, держит 3 штуки (ротация — до записи нового; недоснапшот удаляется), проверяет `integrity_check`.
- Покрывает битую БД/ошибку оператора, **не смерть диска** (тот же диск, SPOF).
- Секреты в snapshot не входят: архивировать `secrets/` отдельно, защищёнными файлами.
- Restore drill — отдельная копия, не поверх production: остановить соответствующий msgd, убрать WAL/SHM цели, восстановить DB + blobs, запустить **совместимую schema ревизию**, проверить `dbversion`/`user-list`. Schema4 snapshot нельзя открыть новым schema5 binary; downgrade/migration не обещаны.

## Ротация carrier-серта / noise-ключа (меняет пины)

Отдельный согласованный rollout с backup/rollback: новые readonly secret files (noise_key0400, owner65532), joint restart, health/Noise-DNS smoke, новый `server-code`. Existing core pins immutable: импорт другого сертификата/Noise key отклонён. Seamless pin rotation сейчас не реализована, не обходить её wipe/silent device replacement. `invite-issue` не обновляет trust и не восстанавливает историю.

## Диск, логи, перезапуск

- Следить за местом: `docker system df`, `du -sh` volumes. Snapshot cap3; за ростом msgd-data следить отдельно.
- `docker logs 53-1`: только bounded факты/статические ошибки, не credentials/ключи. Diagnostic/build tools запускать clean allowlist environment; не печатать полное окружение.
- Топология 2026-10-06: независимые network namespaces. `slipstream` (172.18.0.2) и
  `msgd` (статический `DMSG_MSGD_IPV4`, listen `${DMSG_MSGD_IPV4}:7000`) живут в
  одной выделенной bridge-сети; carrier подключается к фиксированному private
  IP, TCP7000 наружу не публикуется. `depends_on: service_healthy` задаёт
  первоначальный порядок msgd → carrier.
- Рестарт каждого сервиса независим (`restart: unless-stopped`): падение/рестарт
  carrier не трогает msgd, падение/пересоздание msgd не трогает carrier и не
  меняет адрес backend. Проверено 2026-10-06 на физической паре: kill -9 каждого
  сервиса по отдельности и `--force-recreate --no-deps msgd` — обе стороны
  восстановились обычными reconnect/retry, received1 then0, Delivered, skips0.
- Историческая проблема общего namespace (2026-10-06 ранее): отдельный restart
  carrier оставлял живой msgd в старом namespace без loopback backend —
  устранена переходом на независимые namespaces, не требуется совместный
  `--force-recreate slipstream msgd`.
- QUIC loss/backoff допускает временные Transport errors: bounded eventual
  recovery обычными reconnect-командами — норма, не гарантия успеха первого
  reconnect.

## Пределы (честно)

- 1 endpoint, пилот ≤16, transport max-connections32; msgd slots не защищают QUIC admission.
- Kill ≠ power loss: WAL+FULL probes проверяют commit→ACK, не обесточивание.
- Старые P4/P5 DNS и schema4 rebind результаты исторические, не доказательство wire2 rollout.
- Звонки/HA/федерации/группы не обещаны; исходный tunnel не менять.
