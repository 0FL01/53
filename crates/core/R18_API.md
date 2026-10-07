# R18 local text data API

Implemented in `src/history.rs`, exported by `src/ffi.rs::DmsgClient` through
UniFFI proc-macro metadata. Records/enums are re-exported from `ffi` for Rust
callers. Kotlin bindings must be regenerated from the host cdylib by integration.

```rust
history_page(contact_id: String, before_local_id: Option<i64>, limit: u32)
    -> Result<HistoryPage, FfiError>
timeline_page(contact_id: String, before_local_id: Option<i64>, limit: u32)
    -> Result<HistoryPage, FfiError>
history_message(contact_id: String, local_id: i64)
    -> Result<HistoryMessage, FfiError>
message_status(message_id_hex: String)
    -> Result<Option<DeliveryState>, FfiError>
dialogs_page(cursor: Option<String>, limit: u32)
    -> Result<DialogsPage, FfiError>
set_contact_alias(contact_id: String, alias: Option<String>)
    -> Result<(), FfiError>
mark_read(contact_id: String, through_local_id: i64)
    -> Result<i64, FfiError>
```

All calls above are local synchronous storage operations. `Core` has equivalent
methods using borrowed strings and `HistoryError`; the `history` module also
provides connection-level functions. `HistoryError::{InvalidInput,UnknownContact,
Store}` maps to the corresponding safe `FfiError` variants.

## Records

```rust
MessageDirection { Incoming, Outgoing }
DeliveryState { Queued, Accepted, Delivered }

HistoryMessage {
    local_id: i64,
    message_id_hex: String,
    contact_id: String,
    direction: MessageDirection,
    text: String,
    local_timestamp_ms: i64,
    delivery_state: Option<DeliveryState>,
    server_seq: Option<i64>,
    server_timestamp_ms: Option<i64>,
}
HistoryPage {
    rows: Vec<HistoryMessage>,
    next_before_local_id: Option<i64>,
}
DialogSummary {
    contact_id: String,
    local_alias: Option<String>,
    preview: Option<String>,
    last_local_timestamp_ms: Option<i64>,
    local_unread: u64,
    read_cursor: i64,
    has_keys: bool,
    identity_mismatch: bool,
    state: String,
}
DialogsPage {
    rows: Vec<DialogSummary>,
    next_cursor: Option<String>,
}
```

## UI semantics

- Page limits clamp to **1..=100**, as in the existing core API.
- `history_page` remains **local ID descending**: append-only ingestion order
  for exact send reconciliation, not presentation chronology.
- `timeline_page` is newest first: queued local-ID tail, confirmed **original
  server seq descending**, then unavailable legacy metadata by local ID.
  Reverse for displayed bubbles. Start with `None`; exclusive
  `next_before_local_id` resolves the anchor's current timeline position, not
  numeric ID order. `None` next means exhausted. `history_message` refreshes an
  exact retained row by stable ID. All history reads leave read cursors unchanged.
- Pending confirmation can move a stable row between pages. UI refreshes retained
  rows, discovers appends through the separate ingestion watermark, bridges
  canonical gaps and preserves the visible local-ID/offset anchor. A bridge page
  must not advance ingestion past unseen late receives.
- IDs are positive durable SQLite local IDs, shared across contacts. A supplied
  history/read anchor must exist in the requested contact; zero, negatives,
  nonexistent/future and cross-contact IDs return `InvalidInput`.
- Dialogs include **all known contacts**, including empty/requested/blocked
  contacts. Order: local activity milliseconds **descending**, contact ID
  **ascending** for ties. Activity is the maximum of local creation/message
  times. Read/alias/status/trust changes do not promote a dialog. The opaque
  versioned cursor is bounded/canonical and validates time/ID fields.
- Pagination is keyset-based, not a frozen multi-call snapshot. Restart dialog
  paging at `None` after send/fetch/contact additions change activity. A history
  insert above an older-page anchor does not duplicate older rows.
- `local_timestamp_ms` remains this device's queue/decrypt time, unchanged on
  confirmation/upgrade. Dialog activity remains local. These are not server times
  or read receipts; pre-epoch/overflow local clocks fail the write.
- Paired `server_seq`/`server_timestamp_ms` come from the authenticated server's
  durable acceptance metadata (seconds represented as milliseconds). Dedup/retry
  cannot replace them. Confirmed bubbles show server time, queued bubbles local
  queue time. Missing/TTL-expired legacy metadata gets an honest local-time label,
  not a fabricated server timestamp or a restored missing message body.
- Preview is the latest local history row, truncated to 160 Unicode scalar
  values. Empty dialogs have `None` preview/time. Alias is trimmed, 1..=128 UTF-8
  bytes, without control characters; `None` clears it, invalid values are errors.
- `local_unread` counts incoming rows beyond this contact's `read_cursor`, which
  starts at zero. Call `mark_read` only through a row actually viewed. Both
  incoming and outgoing viewed anchors are accepted. The returned cursor is
  monotonic; older valid anchors are idempotent. No read receipt is transmitted.
- Incoming rows have `delivery_state=None`. Outgoing status is exact, persistent
  and monotonic: fresh Queued until valid SEND_ACK plus its original metadata, Accepted on ST_ACCEPTED,
  Delivered on ST_DELIVERED. Transport errors retain the last known state.
  `message_status` accepts exactly 32 ASCII hex characters (case-insensitive),
  returns `None` for unknown/incoming-only IDs, and never interprets absent
  outbox rows as delivered. Delivered history survives outbox removal.
- `has_keys` requires all four pinned routing/identity values. Any presented
  `seen_*` value sets `identity_mismatch`. `state` is a string: legacy
  `requested`/`accepted`/`blocked`, plus one-QR `inviting` (durable outgoing
  request), `incoming` (awaiting local consent) and `accepted_server` (explicit
  consent to server-sourced keys, not personally QR-verified). Metadata never
  bypasses block/pin gates. Native UI and backend must be updated for one-QR
  requests; this does not make an old client understand incoming consent.
  `contact_qr_id` parses without mutation; `invite_contact_qr` persists the
  QR-approved request for existing reconnect/fetch retry. Legacy local-only
  `add_contact_qr` remains available to harnesses.

## Storage and atomicity

Core local schema is **7**. The explicitly approved **6→7** upgrade verifies the
encrypted storage marker/key first, then atomically adds three ordering columns
and indexes in an IMMEDIATE transaction. Identity/account, encrypted text,
ratchets, local IDs/timestamps and read cursors are not rewritten. Wrong keys,
nonempty/unversioned, all other older and future schemas fail before writes;
no generic migration, aliases or wipe. An older core6 APK cannot reopen upgraded
data; do not downgrade/reset it as rollback. Wire2/server schema5 stay unchanged;
deploy metadata-capable backend before this client.
Same-current-schema plain-to-sealed conversion remains supported for Rust
harness storage; Android uses `open_encrypted` with its Keystore-wrapped key.

History text and aliases use `dmsg_seal`/`dmsg_unseal` with separate authenticated
field domains; sealing cleanup and stale-plaintext-write guards cover both.
Plaintext history/alias/preview are excluded from Rust Debug representations.
Outgoing history is in the same transaction as ratchet and ciphertext outbox;
incoming history is in the account/session/inbox-dedup transaction before ACK.
Retry reuses ciphertext/message ID, bypasses history insertion, and cannot
regress status. Status updates commit outbox/history together.

New incoming history stores validated server order in its decrypt transaction
before ACK; SEND promotion stores order/status together. Existing reconnect/fetch
restores at most 32 unchecked legacy rows per call, records unavailable results
without starving later batches, and never silently changes a known order. No
extra worker or periodic full-history scan is introduced.
