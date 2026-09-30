# msgd account authentication

`msgd` uses the shared `dmsg-protocol` version 2 parsers. The device key is
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

## Local control CLI

Set `MSGCTL_SOCK` to the owner-only control socket when using a nondefault path.

```text
msgd msgctl server-code
msgd msgctl registration-mode [open|invite_only]
msgd msgctl invite-issue --out-file <new-file> [ttl_secs]
msgd msgctl invite-revoke --file <invitation-file>
msgd msgctl invite-list
msgd msgctl device-block --file <device-key-hex-file>
msgd msgctl device-unblock --file <device-key-hex-file>
msgd msgctl user-list
msgd msgctl quotas [contact-prefix]
msgd msgctl gc
msgd msgctl backup
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

## Storage and verification

Only fresh schema 5 is supported. Existing empty version 0 can initialize;
nonempty version 0, versions 1–4, and future versions are rejected during a
read-only preflight before writable open or WAL setup. Schema 5 has unique
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
