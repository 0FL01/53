//! R18 local text history. Times are local wall-clock milliseconds, never sender
//! timestamps. Read cursors/unread counts stay on this device, not on the wire.
//! Content and aliases use the existing authenticated column-sealing functions.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::contacts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MessageDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeliveryState {
    Queued,
    Accepted,
    Delivered,
}

/// Positive durable local ID orders the timeline. Incoming delivery_state is
/// always None; outgoing status is a persisted server-ACK fact (or Queued).
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct HistoryMessage {
    pub local_id: i64,
    pub message_id_hex: String,
    pub contact_id: String,
    pub direction: MessageDirection,
    pub text: String,
    pub local_timestamp_ms: i64,
    pub delivery_state: Option<DeliveryState>,
}

impl std::fmt::Debug for HistoryMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryMessage")
            .field("local_id", &self.local_id)
            .field("direction", &self.direction)
            .field("delivery_state", &self.delivery_state)
            .finish_non_exhaustive()
    }
}

/// Newest first (local_id DESC), exclusive next_before_local_id. Reverse rows
/// for chronological rendering; prepend reversed older pages. None = exhausted.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HistoryPage {
    pub rows: Vec<HistoryMessage>,
    pub next_before_local_id: Option<i64>,
}

/// Every known contact appears, including requested/blocked and empty dialogs.
/// Preview is at most 160 Unicode scalar values from the latest local history
/// row. None preview/time means no messages; local_alias None means use the ID.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct DialogSummary {
    pub contact_id: String,
    pub local_alias: Option<String>,
    pub preview: Option<String>,
    pub last_local_timestamp_ms: Option<i64>,
    pub local_unread: u64,
    pub read_cursor: i64,
    pub has_keys: bool,
    pub identity_mismatch: bool,
    pub state: String,
}

impl std::fmt::Debug for DialogSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogSummary")
            .field("local_unread", &self.local_unread)
            .field("read_cursor", &self.read_cursor)
            .field("has_keys", &self.has_keys)
            .field("identity_mismatch", &self.identity_mismatch)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Keyset ordering: local activity DESC, contact ID ASC to break equal times.
/// Activity is max(contact creation time, committed message times); alias/read/
/// ACK changes do not reorder dialogs. Cursors are opaque canonical versioned
/// tokens. Each call is a fresh SQLite snapshot: restart at None after activity
/// changes, rather than treating a paginated list as a frozen live snapshot.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DialogsPage {
    pub rows: Vec<DialogSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    InvalidInput,
    UnknownContact,
    Store,
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid local history input",
            Self::UnknownContact => "unknown contact",
            Self::Store => "local history storage failed",
        })
    }
}

impl std::error::Error for HistoryError {}

impl From<rusqlite::Error> for HistoryError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Store
    }
}

/// No network clock or additional dependency. Out-of-range/pre-epoch clocks
/// fail the transaction instead of overflowing a signed SQLite timestamp.
pub(crate) fn local_time_ms() -> Result<i64, String> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "local clock before epoch")?;
    i64::try_from(elapsed.as_millis()).map_err(|_| "local clock overflow".into())
}

/// Requires the caller's ratchet/outbox/inbox transaction. Strict insertion:
/// retries and durable incoming dedup must bypass this function entirely.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    message_id: &[u8; 16],
    contact_id: &str,
    sender_device: Option<&[u8; 32]>,
    text: &str,
) -> Result<(), String> {
    let now = local_time_ms()?;
    let incoming = sender_device.is_some();
    tx.execute(
        "INSERT INTO core_history(message_id,contact_id,direction,sender_device,text,local_timestamp_ms,delivery_state)
         VALUES(?1,?2,?3,?4,dmsg_seal('history_text',?5),?6,?7)",
        params![message_id.as_slice(), contact_id,
            if incoming { "incoming" } else { "outgoing" },
            sender_device.map(|s| s.as_slice()), text, now,
            if incoming { None } else { Some("queued") }],
    ).map_err(|_| "history insert failed")?;
    let changed = tx.execute(
        "UPDATE core_contacts SET local_activity_ms=max(local_activity_ms,?2) WHERE contact_id=?1",
        params![contact_id, now],
    ).map_err(|_| "dialog activity update failed")?;
    if changed != 1 {
        return Err("history contact missing".into());
    }
    Ok(())
}

fn require_contact(conn: &Connection, contact_id: &str) -> Result<(), HistoryError> {
    if !contacts::valid_contact_id(contact_id) {
        return Err(HistoryError::InvalidInput);
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_contacts WHERE contact_id=?1)",
        [contact_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(HistoryError::UnknownContact);
    }
    Ok(())
}

fn require_anchor(conn: &Connection, contact_id: &str, local_id: i64) -> Result<(), HistoryError> {
    if local_id <= 0 {
        return Err(HistoryError::InvalidInput);
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_history WHERE local_id=?1 AND contact_id=?2)",
        params![local_id, contact_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(HistoryError::InvalidInput);
    }
    Ok(())
}

fn state(value: &str) -> Result<DeliveryState, HistoryError> {
    match value {
        "queued" => Ok(DeliveryState::Queued),
        "accepted" => Ok(DeliveryState::Accepted),
        "delivered" => Ok(DeliveryState::Delivered),
        _ => Err(HistoryError::Store),
    }
}

pub fn history_page(
    conn: &Connection,
    contact_id: &str,
    before_local_id: Option<i64>,
    limit: u32,
) -> Result<HistoryPage, HistoryError> {
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Deferred)?;
    require_contact(&tx, contact_id)?;
    if let Some(anchor) = before_local_id {
        require_anchor(&tx, contact_id, anchor)?;
    }
    let limit = limit.clamp(1, 100) as usize;
    let mut stmt = tx.prepare(
        "SELECT local_id,message_id,direction,CAST(dmsg_unseal('history_text',text) AS TEXT),local_timestamp_ms,delivery_state
         FROM core_history WHERE contact_id=?1 AND local_id<=?2
         ORDER BY local_id DESC LIMIT ?3",
    )?;
    // A validated positive anchor makes subtract-one safe and lets SQLite use
    // the contact/local_id index range directly, even for deep older pages.
    let upper = before_local_id.map(|id| id - 1).unwrap_or(i64::MAX);
    let mut rows = stmt.query(params![contact_id, upper, (limit + 1) as i64])?;
    let mut out = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next()? {
        let message_id: Vec<u8> = row.get(1)?;
        if message_id.len() != 16 {
            return Err(HistoryError::Store);
        }
        let direction = match row.get::<_, String>(2)?.as_str() {
            "incoming" => MessageDirection::Incoming,
            "outgoing" => MessageDirection::Outgoing,
            _ => return Err(HistoryError::Store),
        };
        let delivery = row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(state)
            .transpose()?;
        if (direction == MessageDirection::Outgoing) != delivery.is_some() {
            return Err(HistoryError::Store);
        }
        out.push(HistoryMessage {
            local_id: row.get(0)?,
            message_id_hex: hex(&message_id),
            contact_id: contact_id.into(),
            direction,
            text: row.get(3)?,
            local_timestamp_ms: row.get(4)?,
            delivery_state: delivery,
        });
    }
    let next = if out.len() > limit {
        out.pop();
        out.last().map(|r| r.local_id)
    } else {
        None
    };
    Ok(HistoryPage {
        rows: out,
        next_before_local_id: next,
    })
}

/// Exact outgoing status; None for an unknown ID or incoming-only ID. Never
/// infer delivery from a missing outbox entry. Delivered history is retained.
pub fn message_status(
    conn: &Connection,
    message_id_hex: &str,
) -> Result<Option<DeliveryState>, HistoryError> {
    let mid = parse_message_id(message_id_hex)?;
    let value: Option<String> = conn
        .query_row(
            "SELECT delivery_state FROM core_history WHERE message_id=?1 AND direction='outgoing'",
            [mid.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    value.as_deref().map(state).transpose()
}

/// Local only. Alias is trimmed, 1..=128 UTF-8 bytes, with no control chars;
/// None clears it. Alias updates do not change trust, timestamps or order.
pub fn set_contact_alias(
    conn: &Connection,
    contact_id: &str,
    alias: Option<&str>,
) -> Result<(), HistoryError> {
    let alias = alias.map(str::trim);
    if alias.is_some_and(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control)) {
        return Err(HistoryError::InvalidInput);
    }
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    require_contact(&tx, contact_id)?;
    match alias {
        Some(value) => {
            tx.execute("UPDATE core_contacts SET local_alias=dmsg_seal('contact_alias',?2) WHERE contact_id=?1", params![contact_id,value])?;
        }
        None => {
            tx.execute(
                "UPDATE core_contacts SET local_alias=NULL WHERE contact_id=?1",
                [contact_id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Mark viewed rows through an existing incoming OR outgoing history row in
/// this contact. Never accepts a fabricated future/foreign cursor; older viewed
/// anchors are idempotent. Returns the durable monotonic local cursor.
pub fn mark_read(
    conn: &Connection,
    contact_id: &str,
    through_local_id: i64,
) -> Result<i64, HistoryError> {
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    require_contact(&tx, contact_id)?;
    require_anchor(&tx, contact_id, through_local_id)?;
    tx.execute(
        "UPDATE core_contacts SET read_cursor=max(read_cursor,?2) WHERE contact_id=?1",
        params![contact_id, through_local_id],
    )?;
    let cursor = tx.query_row(
        "SELECT read_cursor FROM core_contacts WHERE contact_id=?1",
        [contact_id],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(cursor)
}

pub fn dialogs_page(
    conn: &Connection,
    cursor: Option<&str>,
    limit: u32,
) -> Result<DialogsPage, HistoryError> {
    let anchor = cursor.map(parse_dialog_cursor).transpose()?;
    let limit = limit.clamp(1, 100) as usize;
    let mut stmt = conn.prepare(
        "SELECT c.contact_id,
            CASE WHEN c.local_alias IS NULL THEN NULL ELSE CAST(dmsg_unseal('contact_alias',c.local_alias) AS TEXT) END,
            CASE WHEN h.text IS NULL THEN NULL ELSE CAST(dmsg_unseal('history_text',h.text) AS TEXT) END,
            h.local_timestamp_ms,c.read_cursor,
            (SELECT count(*) FROM core_history u WHERE u.contact_id=c.contact_id AND u.direction='incoming' AND u.local_id>c.read_cursor),
            c.user_id IS NOT NULL AND c.device_key IS NOT NULL AND c.ed_identity IS NOT NULL AND c.curve_identity IS NOT NULL,
            c.seen_user IS NOT NULL OR c.seen_device IS NOT NULL OR c.seen_ed IS NOT NULL OR c.seen_curve IS NOT NULL,
            c.state,c.local_activity_ms
         FROM core_contacts c LEFT JOIN core_history h ON h.local_id=(SELECT local_id FROM core_history WHERE contact_id=c.contact_id ORDER BY local_id DESC LIMIT 1)
         WHERE (?1 IS NULL OR c.local_activity_ms<?1 OR (c.local_activity_ms=?1 AND c.contact_id>?2))
         ORDER BY c.local_activity_ms DESC,c.contact_id ASC LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![
        anchor.as_ref().map(|a| a.0),
        anchor.as_ref().map(|a| a.1.as_str()),
        (limit + 1) as i64
    ])?;
    let mut out = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next()? {
        let unread: i64 = row.get(5)?;
        let preview: Option<String> = row.get(2)?;
        out.push((
            DialogSummary {
                contact_id: row.get(0)?,
                local_alias: row.get(1)?,
                preview: preview.map(|s| s.chars().take(160).collect()),
                last_local_timestamp_ms: row.get(3)?,
                read_cursor: row.get(4)?,
                local_unread: u64::try_from(unread).map_err(|_| HistoryError::Store)?,
                has_keys: row.get(6)?,
                identity_mismatch: row.get(7)?,
                state: row.get(8)?,
            },
            row.get::<_, i64>(9)?,
        ));
    }
    let next = if out.len() > limit {
        out.pop();
        out.last().map(|(s, t)| dialog_cursor(*t, &s.contact_id))
    } else {
        None
    };
    Ok(DialogsPage {
        rows: out.into_iter().map(|(s, _)| s).collect(),
        next_cursor: next,
    })
}

fn dialog_cursor(time: i64, id: &str) -> String {
    let mut raw = Vec::with_capacity(21);
    raw.push(1);
    raw.extend_from_slice(&time.to_be_bytes());
    raw.extend_from_slice(id.as_bytes());
    URL_SAFE_NO_PAD.encode(raw)
}

fn parse_dialog_cursor(value: &str) -> Result<(i64, String), HistoryError> {
    if value.len() != 28 {
        return Err(HistoryError::InvalidInput);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HistoryError::InvalidInput)?;
    if raw.len() != 21 || raw[0] != 1 || URL_SAFE_NO_PAD.encode(&raw) != value {
        return Err(HistoryError::InvalidInput);
    }
    let time = i64::from_be_bytes(
        raw[1..9]
            .try_into()
            .map_err(|_| HistoryError::InvalidInput)?,
    );
    let id = std::str::from_utf8(&raw[9..]).map_err(|_| HistoryError::InvalidInput)?;
    if time < 0 || !contacts::valid_contact_id(id) {
        return Err(HistoryError::InvalidInput);
    }
    Ok((time, id.into()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_message_id(value: &str) -> Result<[u8; 16], HistoryError> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(HistoryError::InvalidInput);
    }
    let mut out = [0; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .map_err(|_| HistoryError::InvalidInput)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    const A: &str = "ALICE0000001";
    const B: &str = "BOBB00000002";
    const C: &str = "CAROL0000003";
    const KEY: [u8; 32] = [63; 32];

    fn fixture(name: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmsg-history-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = store::open_encrypted(&dir.join("core.db"), &KEY).unwrap();
        for id in [A, B, C] {
            contacts::request_add(&conn, id).unwrap();
        }
        (conn, dir)
    }

    fn append(conn: &Connection, id: &str, n: u32, incoming: bool, text: &str) -> i64 {
        let mut mid = [0; 16];
        mid[..4].copy_from_slice(&n.to_be_bytes());
        let tx =
            Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).unwrap();
        if !incoming {
            store::outbox_insert(&tx, &mid, id, b"ciphertext", "queued").unwrap();
        }
        insert(
            &tx,
            &mid,
            id,
            if incoming { Some(&[7; 32]) } else { None },
            text,
        )
        .unwrap();
        let local_id = tx.last_insert_rowid();
        tx.commit().unwrap();
        local_id
    }

    #[test]
    fn per_contact_bounded_keyset_pages_are_descending_and_validate_anchors() {
        let (conn, dir) = fixture("paging");
        let mut expected = Vec::new();
        for n in 1..=205 {
            expected.push(append(&conn, A, n, n % 2 == 0, &format!("message {n}")));
            if n % 10 == 0 {
                append(&conn, B, 1000 + n, true, "other contact");
            }
        }
        expected.reverse();
        let first = history_page(&conn, A, None, u32::MAX).unwrap();
        assert_eq!(first.rows.len(), 100);
        assert_eq!(history_page(&conn, A, None, 0).unwrap().rows.len(), 1);
        assert_eq!(first.rows[0].text, "message 205");
        assert_eq!(first.rows[0].delivery_state, Some(DeliveryState::Queued));
        assert_eq!(first.rows[1].direction, MessageDirection::Incoming);
        assert_eq!(first.rows[1].delivery_state, None);
        let mut seen: Vec<_> = first.rows.iter().map(|r| r.local_id).collect();
        let mut cursor = first.next_before_local_id;
        // New inserts above the current anchor do not duplicate/skip older rows.
        append(&conn, A, 206, false, "later insert");
        while let Some(anchor) = cursor {
            let page = history_page(&conn, A, Some(anchor), 100).unwrap();
            assert!(page.rows.len() <= 100);
            assert!(page
                .rows
                .iter()
                .all(|r| r.contact_id == A && r.local_id < anchor));
            seen.extend(page.rows.iter().map(|r| r.local_id));
            cursor = page.next_before_local_id;
        }
        assert_eq!(seen, expected);
        assert!(history_page(&conn, C, None, 10).unwrap().rows.is_empty());
        let foreign = history_page(&conn, B, None, 1).unwrap().rows[0].local_id;
        for bad in [i64::MIN, -1, 0, i64::MAX, foreign] {
            assert_eq!(
                history_page(&conn, A, Some(bad), 10),
                Err(HistoryError::InvalidInput)
            );
            assert_eq!(mark_read(&conn, A, bad), Err(HistoryError::InvalidInput));
        }
        assert_eq!(
            history_page(&conn, "short", None, 10),
            Err(HistoryError::InvalidInput)
        );
        assert_eq!(
            history_page(&conn, "UNKNOWN00001", None, 10),
            Err(HistoryError::UnknownContact)
        );
        drop(conn);
        let reopened = store::open_encrypted(&dir.join("core.db"), &KEY).unwrap();
        assert_eq!(
            history_page(&reopened, A, None, 1).unwrap().rows[0].text,
            "later insert"
        );
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dialog_keysets_handle_equal_times_and_reject_malformed_overflow_cursors() {
        let (conn, dir) = fixture("dialogs");
        append(&conn, A, 1, false, &"界".repeat(200));
        append(&conn, B, 2, true, "newest contact");
        conn.execute("UPDATE core_contacts SET local_activity_ms=100", [])
            .unwrap();
        conn.execute(
            "UPDATE core_contacts SET local_activity_ms=200 WHERE contact_id=?1",
            [B],
        )
        .unwrap();
        contacts::block(&conn, C).unwrap();
        let page = dialogs_page(&conn, None, 1).unwrap();
        assert_eq!(page.rows[0].contact_id, B);
        let page2 = dialogs_page(&conn, page.next_cursor.as_deref(), 1).unwrap();
        assert_eq!(page2.rows[0].contact_id, A);
        assert_eq!(page2.rows[0].preview.as_ref().unwrap().chars().count(), 160);
        let page3 = dialogs_page(&conn, page2.next_cursor.as_deref(), 1).unwrap();
        assert_eq!(page3.rows[0].contact_id, C);
        assert_eq!(page3.rows[0].state, "blocked");
        assert_eq!(page3.rows[0].preview, None);
        assert_eq!(page3.rows[0].last_local_timestamp_ms, None);
        assert!(page3.next_cursor.is_none());
        for bad in [
            "".into(),
            "!".repeat(28),
            format!("{}=", page.next_cursor.unwrap()),
            dialog_cursor(-1, A),
            dialog_cursor(i64::MIN, A),
            dialog_cursor(1, "INVALID#0001"),
        ] {
            assert_eq!(
                dialogs_page(&conn, Some(&bad), 10),
                Err(HistoryError::InvalidInput)
            );
        }
        let mut raw = URL_SAFE_NO_PAD.decode(dialog_cursor(100, A)).unwrap();
        raw[0] = 9;
        assert_eq!(
            dialogs_page(&conn, Some(&URL_SAFE_NO_PAD.encode(raw)), 10),
            Err(HistoryError::InvalidInput)
        );
        // Local message activity promotes the dialog, read/alias do not.
        append(&conn, A, 3, true, "recent incoming");
        assert_eq!(dialogs_page(&conn, None, 0).unwrap().rows[0].contact_id, A);
        let before = dialogs_page(&conn, None, 100).unwrap();
        set_contact_alias(&conn, A, Some("New name")).unwrap();
        let last = history_page(&conn, A, None, 1).unwrap().rows[0].local_id;
        mark_read(&conn, A, last).unwrap();
        let after = dialogs_page(&conn, None, u32::MAX).unwrap();
        assert_eq!(
            before
                .rows
                .iter()
                .map(|s| &s.contact_id)
                .collect::<Vec<_>>(),
            after.rows.iter().map(|s| &s.contact_id).collect::<Vec<_>>()
        );
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encrypted_alias_preview_read_cursor_and_trust_state_survive_restart_without_plaintext() {
        let (conn, dir) = fixture("metadata");
        let first = append(&conn, A, 1, true, "first private incoming sentinel");
        let outgoing = append(&conn, A, 2, false, "private outgoing sentinel");
        let latest = append(&conn, A, 3, true, "latest private incoming sentinel");
        let qr = contacts::build_qr(A, &[8; 16], &[9; 32], &[10; 32], &[11; 32]).unwrap();
        contacts::add_from_qr(&conn, &qr).unwrap();
        contacts::confirm_identity(&conn, A).unwrap();
        contacts::accept(&conn, A).unwrap();
        contacts::note_presented_ed(&conn, A, &[12; 32]).unwrap();
        let original = contacts::get(&conn, A).unwrap();
        set_contact_alias(&conn, A, Some("  private alias sentinel  ")).unwrap();
        let summary = || {
            dialogs_page(&conn, None, 100)
                .unwrap()
                .rows
                .into_iter()
                .find(|r| r.contact_id == A)
                .unwrap()
        };
        assert_eq!(summary().local_unread, 2);
        assert_eq!(mark_read(&conn, A, outgoing).unwrap(), outgoing);
        assert_eq!(mark_read(&conn, A, first).unwrap(), outgoing);
        assert_eq!(summary().local_unread, 1);
        assert_eq!(mark_read(&conn, A, latest).unwrap(), latest);
        assert_eq!(summary().local_unread, 0);
        let final_summary = summary();
        assert_eq!(
            final_summary.local_alias.as_deref(),
            Some("private alias sentinel")
        );
        assert!(final_summary.has_keys && final_summary.identity_mismatch);
        assert_eq!(
            contacts::get(&conn, A).unwrap(),
            original,
            "metadata must not change trust"
        );
        for alias in [
            "".into(),
            "   ".into(),
            "x\ny".into(),
            "x\0y".into(),
            "界".repeat(43),
        ] {
            assert_eq!(
                set_contact_alias(&conn, A, Some(&alias)),
                Err(HistoryError::InvalidInput)
            );
        }
        let debug = format!(
            "{:?} {:?}",
            history_page(&conn, A, None, 100).unwrap(),
            dialogs_page(&conn, None, 100).unwrap()
        );
        assert!(!debug.contains("private") && !debug.contains(A));
        let assert_sealed = || {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                for secret in [
                    "first private incoming sentinel",
                    "private outgoing sentinel",
                    "latest private incoming sentinel",
                    "private alias sentinel",
                ] {
                    assert!(
                        !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                        "plaintext in SQLite files"
                    );
                }
            }
        };
        assert_sealed(); // Also inspect active WAL, not just the checkpointed DB.
        drop(conn);
        assert_sealed();
        let conn = store::open_encrypted(&dir.join("core.db"), &KEY).unwrap();
        assert_eq!(
            dialogs_page(&conn, None, 100)
                .unwrap()
                .rows
                .into_iter()
                .find(|r| r.contact_id == A)
                .unwrap(),
            final_summary
        );
        assert_eq!(history_page(&conn, A, None, 100).unwrap().rows.len(), 3);
        append(&conn, A, 4, true, "incoming after read");
        assert_eq!(
            dialogs_page(&conn, None, 100)
                .unwrap()
                .rows
                .into_iter()
                .find(|r| r.contact_id == A)
                .unwrap()
                .local_unread,
            1
        );
        set_contact_alias(&conn, A, None).unwrap();
        assert!(dialogs_page(&conn, None, 100)
            .unwrap()
            .rows
            .into_iter()
            .find(|r| r.contact_id == A)
            .unwrap()
            .local_alias
            .is_none());
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exact_status_is_monotonic_atomic_and_independent_of_outbox_presence() {
        let (conn, dir) = fixture("status");
        append(&conn, A, 1, false, "outgoing");
        let mut mid = [0; 16];
        mid[..4].copy_from_slice(&1u32.to_be_bytes());
        let id = hex(&mid);
        assert_eq!(
            message_status(&conn, &id).unwrap(),
            Some(DeliveryState::Queued)
        );
        store::outbox_set_status(&conn, &mid, "accepted").unwrap();
        assert_eq!(
            message_status(&conn, &id.to_uppercase()).unwrap(),
            Some(DeliveryState::Accepted)
        );
        conn.execute_batch("CREATE TRIGGER reject_status BEFORE UPDATE OF delivery_state ON core_history BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        assert!(store::outbox_set_status(&conn, &mid, "delivered").is_err());
        assert_eq!(
            store::outbox_queued(&conn, 0, 100).unwrap().0[0].4,
            "accepted"
        );
        assert_eq!(
            message_status(&conn, &id).unwrap(),
            Some(DeliveryState::Accepted)
        );
        conn.execute_batch("DROP TRIGGER reject_status").unwrap();
        store::outbox_set_status(&conn, &mid, "delivered").unwrap();
        for late in ["accepted", "queued"] {
            store::outbox_set_status(&conn, &mid, late).unwrap();
        }
        assert_eq!(
            message_status(&conn, &id).unwrap(),
            Some(DeliveryState::Delivered)
        );
        assert!(store::outbox_queued(&conn, 0, 100).unwrap().0.is_empty());
        conn.execute("DELETE FROM core_outbox", []).unwrap();
        assert_eq!(
            message_status(&conn, &id).unwrap(),
            Some(DeliveryState::Delivered)
        );
        assert_eq!(
            history_page(&conn, A, None, 1).unwrap().rows[0].delivery_state,
            Some(DeliveryState::Delivered)
        );
        assert_eq!(message_status(&conn, &hex(&[99; 16])).unwrap(), None);
        append(&conn, B, 2, true, "incoming");
        let incoming = history_page(&conn, B, None, 1).unwrap().rows[0]
            .message_id_hex
            .clone();
        assert_eq!(message_status(&conn, &incoming).unwrap(), None);
        for bad in [
            "".into(),
            "f".repeat(31),
            "g".repeat(32),
            "f".repeat(33),
            "é".repeat(16),
        ] {
            assert_eq!(message_status(&conn, &bad), Err(HistoryError::InvalidInput));
        }
        drop(conn);
        let conn = store::open_encrypted(&dir.join("core.db"), &KEY).unwrap();
        assert_eq!(
            message_status(&conn, &id).unwrap(),
            Some(DeliveryState::Delivered)
        );
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_and_alias_authentication_fail_closed_without_resealing_or_sensitive_errors() {
        let (conn, dir) = fixture("authentication");
        append(&conn, A, 1, true, "private history authentication sentinel");
        set_contact_alias(&conn, A, Some("private alias authentication sentinel")).unwrap();
        let sealed_text: Vec<u8> = conn
            .query_row("SELECT text FROM core_history", [], |r| r.get(0))
            .unwrap();
        let sealed_alias: Vec<u8> = conn
            .query_row(
                "SELECT local_alias FROM core_contacts WHERE contact_id=?1",
                [A],
                |r| r.get(0),
            )
            .unwrap();
        // Same format, different authenticated field domains: swapping fails.
        conn.execute("UPDATE core_history SET text=?1", [&sealed_alias])
            .unwrap();
        assert_eq!(history_page(&conn, A, None, 100), Err(HistoryError::Store));
        assert_eq!(dialogs_page(&conn, None, 100), Err(HistoryError::Store));
        conn.execute("UPDATE core_history SET text=?1", [&sealed_text])
            .unwrap();
        let mut damaged = sealed_alias.clone();
        *damaged.last_mut().unwrap() ^= 1;
        conn.execute(
            "UPDATE core_contacts SET local_alias=?2 WHERE contact_id=?1",
            params![A, damaged],
        )
        .unwrap();
        assert_eq!(dialogs_page(&conn, None, 100), Err(HistoryError::Store));
        assert_eq!(
            HistoryError::Store.to_string(),
            "local history storage failed"
        );
        conn.execute(
            "UPDATE core_contacts SET local_alias=?2 WHERE contact_id=?1",
            params![A, sealed_alias],
        )
        .unwrap();
        drop(conn);
        let raw = Connection::open(dir.join("core.db")).unwrap();
        raw.execute_batch("DROP TABLE core_storage").unwrap();
        drop(raw);
        let before = std::fs::read(dir.join("core.db")).unwrap();
        assert!(store::open_encrypted(&dir.join("core.db"), &KEY)
            .unwrap_err()
            .contains("storage marker missing"));
        assert_eq!(std::fs::read(dir.join("core.db")).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
