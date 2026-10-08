# 53 server and account authentication

`53` (internal Cargo package `msgd`) uses shared `dmsg-protocol` version 2 parsers. The device key is
always the authenticated Noise IK initiator static key.

## Wire contract

After Noise IK, send `AUTH_DOMAIN(3)` with the configured domain and receive
`WELCOME(2)`. Account authentication then supports:

| Request | Payload | Response |
| --- | --- | --- |
| `POLICY(7)` | empty | `POLICY_RESP(8)`: `0` invite-only, `1` open |
| `SIGNUP(9)` | `auth::build_signup(login, password, invitation)` | `AUTHENTICATED(12)` or `ERROR(6)` |
| `LOGIN(10)` | `auth::build_login(login, password, expected_old_device)` | `AUTHENTICATED(12)`, `REPLACE_REQUIRED(14)`, or `ERROR(6)` |
| `RESUME(11)` | empty | `AUTHENTICATED(12)` or `ERROR(6)` |

`AUTHENTICATED` is `user_id[16] + contact_id[12]`; `REPLACE_REQUIRED` is the
current device key `[32]`. Credential payloads contain canonical login and
password fields followed by the optional invitation/expected-old-key flag and
value. They contain no new device key. Login names are lowercase ASCII, 3–32
bytes, allowing letters, digits, `_`, `.`, and `-`; passwords are UTF-8,
8–128 bytes, without control characters.

A new device first obtains a replacement challenge with no database mutation.
Explicit confirmation compares the expected old key with the current key in
one transaction. The winner retires the old key, deletes its prekeys, and
starts the new cursor at the mailbox high-water mark. A concurrent loser gets
the new challenge. Same-active-key retries return the original account IDs,
including after a committed response was lost. Retired keys cannot resume or
be reactivated by `device-unblock`. Committed replacement closes live and
pending old-key sessions.

Signup atomically creates the user, device, cursor, and consumes the optional
invitation. Same-key, same-credential signup retries succeed independently of
later policy/invitation changes; wrong same-key credentials return
`ERR_CREDENTIALS`. A fresh device registering an occupied login gets
`ERR_CONFLICT`, independently of its password, including when signup loses a
race. Neither rejection consumes an invitation or changes account/device state.
Invitations are one-time signup authorization, never account credentials.

Password work uses Argon2id v19, 19 MiB, two iterations, one lane, and random
16-byte salts. Hashing/verification runs in `spawn_blocking`, outside the DB
mutex, with two non-queued slots. Missing accounts perform the same-cost dummy
verification and return the same credentials error as wrong passwords.
Attempts are bounded to 32 globally and eight per canonical login/device per
60-second window, with a bounded counter table; no IP/resolver bans are used.

## Authenticated mailbox and binding

`DEVICE_BINDING(28)` accepts exactly `user_id[16]` and returns
`DEVICE_BINDING_RESP(29)` containing `user_id[16] + device[32] + Ed25519[32] +
Curve25519[32]`. This is an exact authenticated active-user lookup. Missing
bindings return `ERR_NO_PREKEY`; there is no login directory.

`UPLOAD_PREKEYS(21)` starts with Ed25519 `[32]`, Curve25519 `[32]`, and a
big-endian count `[2]`, followed by the shared entry format. A zero-entry
upload can establish the binding. Both identity keys are immutable for that
device; a different binding is rejected atomically.

`FETCH_RESP(19)` records contain `seq[8] + sender_device[32] + sender_user[16]
+ message_id[16] + ciphertext_length[2] + ciphertext`. To guarantee delivery,
the server accepts at most the shared `CIPHERTEXT_MAX` ciphertext bytes (16,304); larger
`SEND` requests return `ERR_BAD` without insertion.

Mailbox deduplication precedes quota checks. Retrying the same sender/device
and message ID returns its persisted accepted/delivered status, even at full
quota; changed retry ciphertext does not replace the original event or consume
quota again.

## Client-issued signup invitations

Fresh schema7 supports account-owned `INVITE_ISSUE46/ISSUED47`,
`INVITE_REVOKE48/REVOKED49` and `INVITE_LIST50/LISTED51` after account authentication.
Every command rechecks the exact active/unblocked device and user under the DB lock.
Wire layouts and the shared strict codec are in `docs/protocol.md` and
`crates/protocol/src/invitation.rs`.

Client issue ID is16 bytes, scoped to its owner. Same-ID recovery returns the
original record before quota/rate/RNG, even after restart/use/revoke/expiry;
only Active returns a phrase. Six uniform pinned EFF Long words derive one token:
`SHA256(b"dmsg signup phrase v1\0" || canonical_phrase)`. QR is its existing raw43
encoding; both use unchanged SIGNUP optional32 and consume one row atomically.
Self-service entropy≈77.55bit; operator random32 remains256bit. Source/license:
`crates/protocol/EFF-WORDLIST.md`.

TTL24h, maximum8 active invitations/account. A separate rolling60s successful-commit
budget permits8/account and32/global; replay is free and auth attempts are unchanged.
Quota returns `ERR_INVITE_LIMIT14`, not mailbox quota; rate returns THROTTLED.
LIST is bounded, own-active metadata only. Unknown/foreign revoke is uniformly BAD;
own terminal revoke is no-op. Management state precedence is used/revoked/expired,
without changing existing signup error ordering. Replacement/block of the issuing
device prevents its new commands but does not automatically revoke account-owned
invitations. Terminal rows are retained for durable idempotency: no invite GC or
bounded-total-storage promise. Active phrases are bearer secrets in DB/backups.

The existing private CLI bootstraps the first fresh invite-only account; routine
issuance then happens in the client. Android PNG Share/image-picker acceptance is
separate: `android/SELF_SERVICE_INVITATION_GATES.md`.

## Local control CLI

Set `MSGCTL_SOCK` to the owner-only control socket when using a nondefault path.

```text
53ctl ping
53ctl server-code
53ctl registration-mode [open|invite_only]
53ctl qr-invite [--ttl <seconds> | --file <existing-private-invitation-file>]
53ctl invite-issue --out-file <new-file> [ttl_secs]
53ctl invite-revoke --file <invitation-file>
53ctl invite-list
53ctl device-block --file <device-key-hex-file>
53ctl device-unblock --file <device-key-hex-file>
53ctl user-list
53ctl quotas [contact-prefix]
53ctl gc
53ctl backup
```

`server-code` emits a public `dmsg://server/` profile containing the domain,
carrier certificate DER, and Noise public key. The persisted registration
default is `invite_only`. Invitation output is a standalone 43-character
base64url token in a newly created `0600` file; stdout is only `ok`. Existing
output paths fail before issuance. Failed writes remove the partial file and
attempt to revoke the unusable invitation. Secret inputs are bounded regular
owner-only files. `invite-list` redacts tokens to an eight-byte hex prefix and
reports created/expiry/revoked/used state. User output contains no credentials
or password hashes.

### Administrator: QR in the terminal

```sh
docker exec -it 53-1 53ctl qr-invite              # default 24 hours
docker exec -it 53-1 53ctl qr-invite --ttl 3600
docker exec -it 53-1 53ctl qr-invite --file /var/lib/msgd/invites/example.invite
```

Creates a random-named `0600` file in the existing data volume's `0700` `invites/`
directory and renders its raw token as a fixed-contrast QR with a quiet zone.
Plaintext token is never echoed. `--file` only renders an existing bounded
owner-only invitation; it does not issue or validate one. Both stdin and stdout
must be terminals, otherwise rejection happens **before** issuance. Do not
capture/share terminal recordings: the QR itself is a secret. Docker exec output
is not service logs. For same-phone import, transfer the private file via a
trusted channel; scanning is for another screen/paper. Expiry, revocation and
one-use authorization are still checked by signup.

Container `53-1`, entrypoint `53`, direct control `53ctl`. Internal crate/service,
environment/socket/data-volume names remain unchanged to preserve state/pins.

## Storage and verification

Only fresh schema 7 is supported. Existing empty version 0 can initialize;
nonempty version 0, versions 1–6, and future versions are rejected during a
read-only preflight before writable open or WAL setup. Schema 7 has unique
non-null logins and password hashes, persisted registration mode, and a partial
unique index allowing one nonretired device per account.

```sh
cargo fmt -p msgd
cargo build -p msgd
cargo test -p msgd
```

`auth_probe` covers policy, invite lifecycle, signup/confirmation response
loss, restart/resume, occupied-login signup and races, replacement CAS/wakeup/no-history,
immutable bindings, full-quota persisted retries, and expanded FETCH bounds. Unit tests inject transaction
failures to prove signup/replacement rollback, and verify Argon slot and
attempt bounds. `mailbox_probe`, `backup_probe`, `msgctl_probe`, and
`noise_probe` retain the durable mailbox/GC/backup and transport/control gates.
These are local server probes; DNS/Android acceptance is separate.

## Opaque voice blobs (wire2, fresh server7)

`dmsg_protocol::blob` owns bounded payload builders/parsers and exact DTOs.
RESERVE/RESERVED retain opcodes26/27; STATUS/RESP37/38, PUT/ACK39/40,
FINISH/ACK41/42, GET/DATA43/44, SEND_MEDIA45 (existing SEND_ACK17).
Chunk indices and lengths are `u16 BE`, status is blob16 + size4 + state1
(`Reserved=0`, `Complete=1`) + received `u64 BE` bitmap (bit = index).
Chunks are up to8192 ciphertext bytes; generic blobs up to512KiB/64chunks.
Voice manifest v1/profile1 is E2E kind4, authenticated inside Olm; msgd never
receives its key, waveform, samples or plaintext container. Voice clients cap
the encrypted object at128KiB and encode8176 plaintext bytes +16 AEAD tag per
full chunk. No additional checksum/digest registry is involved.

Uploads belong to the exact authenticated owner user/device. PUT syncs a0600
temporary chunk, renames it and syncs the parent before committing its SQLite
receipt and returning an ID/index-bound ACK. Same index/same bytes is
idempotent; different bytes fail, including durable rename before receipt.
FINISH requires every receipt and exact ciphertext size. STATUS supports
resume after reopen; recipient GET requires completion, a committed exact-device
ACL, an active exact requester device and an accepted, unblocked contact pair.

SEND_MEDIA atomically commits mailbox+ACL. Retries must match original blob,
recipient user/device and Olm ciphertext. A matching retry reports the original
accept/delivery after recipient replacement; it never routes to a replacement
or adds a replacement ACL. FETCH/DELIVERY_ACK also respect exact media device.
Reservations and completed blobs together count against32MiB/512blobs per
owner. Reserved TTL24h; completed TTL7d (retained through accepted mailbox TTL).
`53ctl gc` removes expired metadata before chunks and sweeps orphan files.

One shared IO permit bounds blob disk work and excludes backup/GC races;
contending blob operations return retryable ERR_BUSY. Disk work runs on a
blocking worker with no DB lock across it; separate control streams remain
usable. Backup uses a separate read-only SQLite snapshot, copies exactly its
referenced chunks, verifies sizes/completion/ACL references and fails on missing
data. Secret files remain outside the backup. `blob_probe` covers local live
resume, ACL/replacement, receipt and ACL-transaction failure boundaries,
concurrent control progress, consistent backup references and orphan/TTL GC.
