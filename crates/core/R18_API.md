# R18 local message data API

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
history_message_by_mid(contact_id: String, message_id_hex: String)
    -> Result<Option<HistoryMessage>, FfiError>
message_status(message_id_hex: String)
    -> Result<Option<DeliveryState>, FfiError>
dialogs_page(cursor: Option<String>, limit: u32)
    -> Result<DialogsPage, FfiError>
set_contact_alias(contact_id: String, alias: Option<String>)
    -> Result<(), FfiError>
mark_read(contact_id: String, through_local_id: i64)
    -> Result<i64, FfiError>
edit_message(contact_id: String, local_id: i64, expected_revision: u64, text: String)
    -> Result<HistoryMessage, FfiError>
delete_message(contact_id: String, local_id: i64, scope: DeleteScope)
    -> Result<HistoryMessage, FfiError>
```

All calls above are local synchronous storage operations. The `history` module
provides connection-level read functions; `Core` owns the mutation transaction.
History errors map to safe `InvalidInput/UnknownContact/Store`; mutations also
expose typed `MessageChanged/MessageUnavailable` and existing contact/trust errors.

## Records

```rust
MessageDirection { Incoming, Outgoing }
MessageKind { Text, Voice }
DeliveryState { Queued, Accepted, Delivered }
DeleteScope { SelfOnly, Everyone }
VoiceInfo { sample_count: u32, byte_len: u32, waveform: Vec<u8>, downloaded: bool }

HistoryMessage {
    local_id: i64,
    message_id_hex: String,
    contact_id: String,
    direction: MessageDirection,
    kind: MessageKind,
    text: String,
    voice: Option<VoiceInfo>,
    local_timestamp_ms: i64,
    delivery_state: Option<DeliveryState>,
    server_seq: Option<i64>,
    server_timestamp_ms: Option<i64>,
    revision: u64,
    hidden_self: bool,
    deleted_all: bool,
    change_delivery_state: Option<DeliveryState>,
}
HistoryPage {
    rows: Vec<HistoryMessage>,
    next_before_local_id: Option<i64>,
}
DialogSummary {
    contact_id: String,
    local_alias: Option<String>,
    preview: Option<String>,
    preview_kind: Option<MessageKind>,
    voice_duration_ms: Option<u32>,
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
  server seq descending**. There is no legacy metadata/backfill prefix.
  Reverse for displayed bubbles. Start with `None`; exclusive
  `next_before_local_id` resolves the anchor's current timeline position, not
  numeric ID order. `None` next means exhausted. `history_message` refreshes an
  exact retained row by stable ID. All history reads leave read cursors unchanged.
- Pending confirmation can move a stable row between pages. UI refreshes retained
  rows, discovers appends through the separate ingestion watermark, bridges
  canonical gaps and preserves the visible local-ID/offset anchor. A bridge page
  must not advance ingestion past unseen late receives.
- IDs are positive durable SQLite local IDs, shared across contacts. A supplied
  history/read anchor must be a TEXT/VOICE row in the requested contact; zero, negatives,
  control-event, nonexistent/future and cross-contact IDs return `InvalidInput`.
  Both history APIs retain hidden/deleted base anchors; UI filters them from the
  visible projection. A read anchor must additionally be visible.
- Dialogs include **all known contacts**, including empty/requested/blocked
  contacts. Order: local activity milliseconds **descending**, contact ID
  **ascending** for ties. Activity is the maximum of local creation/message
  times. Read/alias/status/trust changes do not promote a dialog. The opaque
  versioned cursor is bounded/canonical and validates time/ID fields.
- Pagination is keyset-based, not a frozen multi-call snapshot. Restart dialog
  paging at `None` after send/fetch/contact additions change activity. A history
  insert above an older-page anchor does not duplicate older rows.
- `local_timestamp_ms` remains this device's queue/decrypt time, unchanged on
  confirmation/edit/delete. Dialog activity remains local. These are not server times
  or read receipts; pre-epoch/overflow local clocks fail the write.
- Paired `server_seq`/`server_timestamp_ms` come from the authenticated server's
  durable acceptance metadata (seconds represented as milliseconds). Dedup/retry
  cannot replace them. Confirmed bubbles show server time, queued bubbles local
  queue time. Fresh incoming and Accepted/Delivered TEXT/VOICE require real paired
  metadata. Known order is immutable; lost TTL/GC chronology is not fabricated.
- Preview uses the latest visible local TEXT/VOICE row. TEXT is truncated to 160
  Unicode scalar values; VOICE has `preview=None`, typed kind/duration and a
  localized Android label. Voice history likewise uses empty `text`, public
  `VoiceInfo` only (never a key), and exact media state. Empty dialogs have `None`
  preview/time. Alias is trimmed, 1..=128 UTF-8
  bytes, without control characters; `None` clears it, invalid values are errors.
- `local_unread` counts visible incoming TEXT/VOICE beyond this contact's `read_cursor`, which
  starts at zero. Call `mark_read` only through a row actually viewed. Both
  incoming and outgoing viewed anchors are accepted. The returned cursor is
  monotonic; older valid anchors are idempotent. No read receipt is transmitted.
- Incoming rows have `delivery_state=None`. Outgoing status is exact, persistent
  and monotonic: Queued until valid SEND_ACK (TEXT/VOICE also require original metadata;
  controls do not), Accepted on ST_ACCEPTED, Delivered on ST_DELIVERED.
  Transport errors retain the last known state.
  `message_status` accepts exactly 32 ASCII hex characters (case-insensitive),
  returns `None` for unknown/incoming-only IDs, and never interprets absent
  outbox rows as delivered. It accepts any outgoing event MID, including EDIT/
  DELETE after outbox removal; original MID always returns the base status.
  `change_delivery_state` is the latest outgoing control's separate status.
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

Core schema **9** is fresh-only. One `core_messages` table holds base TEXT/VOICE and
internal EDIT/DELETE events, incoming dedup/pending controls and outgoing immutable
ciphertext/status. No history/inbox/outbox aliases, schema upgrades, plain→sealed
conversion or order backfill exist. Encrypted initialization creates marker and
non-NULL sealed-value guards atomically from the first write. Wrong/missing key,
unsupported schema and initialized unmarked plaintext fail without mutation/reset.
Rust-only fresh plaintext harnesses remain separate; Android has only the keyed
constructor and same-version snapshot restore with its original Keystore key.

Nullable sealed bodies are cleared logically; readers never unseal NULL. Text,
voice manifests and aliases use separate authenticated field domains and are omitted from their Rust
Debug views. This is not forensic erasure of SQLite/ciphertext/backups, nor snapshot
rollback prevention. Wire2/auth/carrier are unchanged; blob-capable server6 is
also fresh-only. Working server5/main8 are not upgraded by the voice gate.

Incoming crypto candidates, event/dedup and final projection commit together before
ACK. Final FETCH report rereads the batch's visible effective TEXT/VOICE; controls never
count as new messages. Only unseen base messages need matching acceptance metadata;
replays/controls use the current FETCH seq for ACK without persisting control order.
Outgoing retry preserves MID/ciphertext and frozen full recipient binding, never
re-encrypts for replacement pins, and progresses only that event's status.

## Own-message mutations

- Remote Edit/Everyone require visible own Accepted/Delivered base, unchanged
  recipient binding, sendable contact and cached session matching current pins.
  No implicit CLAIM/network occurs in the local mutation. Edit CAS checks the
  expected revision before the same-text no-op; conflict preserves the UI draft.
  Edit is TEXT-only; Everyone also targets VOICE.
- One IMMEDIATE transaction saves updated base, ratchet and separately identified
  queued control; returned row is decoded before commit. Original MID/local ID/
  timestamps/order/ciphertext remain unchanged; base delivery stays monotonic.
- Higher edits win on receive; terminal delete dominates late edits/originals.
  Unknown targets journal in the same table, retaining only the winning pending
  edit body. Applied/superseded bodies clear; durable event keys remain.
- SelfOnly is local/offline and leaves revision unchanged; no control is created.
  It does not cancel a queued send. Deleted/hidden rows return empty text and flags,
  stay canonical paging anchors, but never appear in bubbles/inbox/preview/unread.
- Outbox exposes typed text/voice/edit/delete events and their own exact status; delete
  delivery remains inspectable when its base bubble is invisible. UI performs
  existing retry after save; network failure means saved/pending, not not-saved.

## Voice commands and transfer boundary

```rust
voice_session_ready(contact_id: String) -> Result<bool, FfiError>
prime_voice_session_dns(contact_id: String) -> Result<(), FfiError> // network
queue_voice(contact_id: String, message_id_hex: String, encoded_note: Vec<u8>)
    -> Result<HistoryMessage, FfiError> // local, stable-MID idempotent
voice_data(contact_id: String, local_id: i64) -> Result<Vec<u8>, FfiError>
pending_voice_upload() -> Result<Option<HistoryMessage>, FfiError>
clear_voice_cache() -> Result<(), FfiError>
prepare_voice_transfer(contact_id: String, local_id: i64, download: bool)
    -> Result<Arc<VoiceTransfer>, FfiError>
VoiceTransfer::advance() -> Result<VoiceTransferProgress, FfiError> // network
commit_voice_transfer(transfer: Arc<VoiceTransfer>)
    -> Result<VoiceTransferProgress, FfiError>
VoiceTransfer::cancel()
// VoiceTransferProgress { transferred: u32, total: u32, complete: bool }
```

- PCM never enters UniFFI; foreground recorder/player use bounded direct JNI.
  Encoded notes are bounded libopus1.6.1 mono16k/10k VBR/20ms containers with exact
  samples/lookahead/trim. NoLACE decoder7, encoder10; DTX/FEC/DRED/BWE off.
- Queue validates actual128KiB final encrypted size and atomically saves base,
  ratchet, immutable Olm event and AEAD chunks. Session bootstrap is explicit;
  first offline note without a session returns `VoiceSessionRequired`, retaining
  the memory preview. Existing pinned session supports offline durable queue.
- `prepare` captures short storage state; `advance` reuses one private Noise
  stream outside Kotlin store-lock/DB transaction; `commit` persists receipts
  under the lock and rechecks epoch/tombstone. No ciphertext/key enters Kotlin.
- FINISH is not SEND_MEDIA. After upload and worker start the existing control
  retry sends queued manifests; incomplete uploads do not block TEXT/DELETE.
- Receive saves manifest before ACK without downloading. Delivered is manifest
  delivery, not downloaded/listened. Download is explicit and resumes receipts
  after process death; authenticated complete data is validated before playback.
- Sealed manifest and encrypted chunks stay inside core.db for existing snapshots.
  Cache32MiB/LRU and explicit clear preserve queued/active chunks. Delete clears
  playable content and rejects late download commits; hidden queued own uploads
  still finish. No physical erasure/server-revoke promise.
