//! Ephemeral call keys over the existing pinned Olm ratchets. Only account and
//! session state may be committed here; call plaintext never enters history.

use dmsg_protocol::call::{Action, KeyEnvelope, CIPHERTEXT_MAX};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use vodozemac::olm::{Account, OlmMessage, Session};
use zeroize::Zeroizing;

use super::{contact_keys, contacts, olm, recipient_binding, Contact, Core, OlmError};

pub(crate) struct Peer {
    pub user: [u8; 16],
    pub device: [u8; 32],
    pub binding: [u8; 32],
}

impl Core {
    /// Coherent local routing/pin snapshot, including any existing session pins.
    /// Network directory refresh and prekey priming belong to the call actor.
    pub(crate) fn call_peer(&self, contact_id: &str) -> Result<Peer, OlmError> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("call peer transaction failed".into()))?;
        let c = call_contact(&tx, contact_id)?;
        call_identity(&tx, &self.device_pub)?;
        pinned_sessions(&tx, &c)?;
        let (user, device, _, _) = contact_keys(&c)?;
        Ok(Peer {
            user,
            device,
            binding: recipient_binding(&c)?,
        })
    }

    /// A new directional key/salt and RTP identity for this Invite or Accept.
    /// No Olm message is consumed until encrypt_call commits successfully.
    pub(crate) fn call_offer(
        &self,
        contact_id: &str,
        call_id: [u8; 16],
        action: Action,
    ) -> Result<KeyEnvelope, OlmError> {
        call_context(&call_id, action)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("call offer transaction failed".into()))?;
        let c = call_contact(&tx, contact_id)?;
        let (account, _, binding) = call_identity(&tx, &self.device_pub)?;
        pinned_sessions(&tx, &c)?;
        let mut envelope = KeyEnvelope {
            call_id,
            sender_ed: olm::ed_identity(&account),
            action,
            epoch: 1,
            sender_binding: binding,
            recipient_binding: recipient_binding(&c)?,
            key: [0; 30],
            ssrc: 0,
            cname: [0; 8],
            sequence: u16::from_be_bytes(random_bytes()?),
            timestamp: u32::from_be_bytes(random_bytes()?),
        };
        if envelope.sender_binding == envelope.recipient_binding {
            return Err(OlmError::IdentityMismatch);
        }
        while envelope.key == [0; 30] {
            getrandom::fill(&mut envelope.key).map_err(|_| OlmError::Crypto("rng"))?;
        }
        while envelope.ssrc == 0 {
            envelope.ssrc = u32::from_be_bytes(random_bytes()?);
        }
        // Rejection sampling avoids biased alphabet selection. CNAME and SSRC
        // collisions with the other direction are checked by MediaConfig.
        const ALPHABET: &[u8; 62] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        for ch in &mut envelope.cname {
            loop {
                let [byte] = random_bytes()?;
                if byte < 248 {
                    *ch = ALPHABET[usize::from(byte % 62)];
                    break;
                }
            }
        }
        Ok(envelope)
    }

    /// Encrypt exactly this offer on an already primed session. The returned
    /// ciphertext is transient: an unsuccessful signal ends this call attempt.
    pub(crate) fn encrypt_call(
        &mut self,
        contact_id: &str,
        envelope: &KeyEnvelope,
    ) -> Result<Vec<u8>, OlmError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("call encrypt transaction failed".into()))?;
        let c = call_contact(&tx, contact_id)?;
        let (account, next_key_id, binding) = call_identity(&tx, &self.device_pub)?;
        if envelope.sender_ed != olm::ed_identity(&account)
            || envelope.sender_binding != binding
            || envelope.recipient_binding != recipient_binding(&c)?
        {
            return Err(OlmError::IdentityMismatch);
        }
        // Strict version/kind/action/call_id/epoch/profile validation happens
        // before touching even the disposable sending ratchet.
        let plain = Zeroizing::new(envelope.encode().map_err(OlmError::Protocol)?);
        let mut sessions = pinned_sessions(&tx, &c)?.ok_or(OlmError::MessageUnavailable)?;
        let wire = olm::encode_wire(
            &sessions[0]
                .encrypt(plain.as_slice())
                .map_err(|_| OlmError::Crypto("call encrypt"))?,
        );
        if wire.len() > CIPHERTEXT_MAX {
            return Err(OlmError::Protocol("call ciphertext too long"));
        }
        let pickle = Zeroizing::new(olm::pickle_sessions(&sessions)?);
        tx.execute(
            "UPDATE core_sessions SET pickle=dmsg_seal('session_pickle',?2) WHERE contact_id=?1",
            params![contact_id, pickle.as_str()],
        )
        .map_err(|_| OlmError::Store("call session write failed".into()))?;
        tx.commit()
            .map_err(|_| OlmError::Store("call encrypt commit failed".into()))?;
        self.account = account;
        self.next_key_id = next_key_id;
        Ok(wire)
    }

    /// Only strict CALL plaintext is accepted. Authentication/context failures
    /// discard candidate state, including a first-message OTK consumption.
    pub(crate) fn decrypt_call(
        &mut self,
        peer_user: [u8; 16],
        peer_device: [u8; 32],
        ciphertext: &[u8],
        call_id: [u8; 16],
        action: Action,
    ) -> Result<(String, KeyEnvelope), OlmError> {
        call_context(&call_id, action)?;
        if ciphertext.is_empty() || ciphertext.len() > CIPHERTEXT_MAX {
            return Err(OlmError::Protocol("invalid call ciphertext length"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("call decrypt transaction failed".into()))?;
        let c = contacts::get_by_user(&tx, &peer_user)?.ok_or(OlmError::UnknownContact)?;
        let c = call_contact(&tx, &c.contact_id)?;
        let (user, device, peer_ed, peer_curve) = contact_keys(&c)?;
        if user != peer_user || device != peer_device {
            return Err(OlmError::IdentityMismatch);
        }
        let (mut account, next_key_id, binding) = call_identity(&tx, &self.device_pub)?;
        let mut sessions = pinned_sessions(&tx, &c)?.unwrap_or_default();
        let msg = olm::decode_wire(ciphertext)?;
        let mut consumed_otk = false;
        let plain = Zeroizing::new(match &msg {
            OlmMessage::PreKey(pre) => {
                let presented = *pre.identity_key().as_bytes();
                if presented != peer_curve {
                    // Preserve pins and the old session. Only the existing
                    // intentional identity-warning model may survive failure.
                    contacts::note_presented_curve(&tx, &c.contact_id, &presented)?;
                    tx.commit()
                        .map_err(|_| OlmError::Store("call warning commit failed".into()))?;
                    return Err(OlmError::IdentityMismatch);
                }
                if let Some(index) = sessions
                    .iter()
                    .position(|s| s.session_id() == pre.session_id())
                {
                    let mut candidate = Session::from_pickle(sessions[index].pickle());
                    let plain = candidate
                        .decrypt(&msg)
                        .map_err(|_| OlmError::Crypto("call decrypt"))?;
                    sessions[index] = candidate;
                    plain
                } else {
                    if sessions.len() >= 2 {
                        return Err(OlmError::Crypto("call session bound"));
                    }
                    // Do not apply another session's prekey to an outgoing
                    // ratchet: simultaneous first TEXT/CALL sends are valid.
                    let mut candidate = Account::from_pickle(account.pickle());
                    let (session, _, plain) = olm::inbound(&mut candidate, &peer_curve, pre)?;
                    account = candidate;
                    consumed_otk = true;
                    sessions.push(session);
                    plain
                }
            }
            OlmMessage::Normal(_) => {
                let mut decrypted = None;
                for session in &mut sessions {
                    let mut candidate = Session::from_pickle(session.pickle());
                    if let Ok(plain) = candidate.decrypt(&msg) {
                        *session = candidate;
                        decrypted = Some(plain);
                        break;
                    }
                }
                decrypted.ok_or(OlmError::Crypto("call decrypt"))?
            }
        });
        let envelope = KeyEnvelope::decode(&plain).map_err(OlmError::Protocol)?;
        if envelope.call_id != call_id
            || envelope.action != action
            || envelope.epoch != 1
            || envelope.sender_binding != recipient_binding(&c)?
            || envelope.recipient_binding != binding
        {
            return Err(OlmError::Protocol("call envelope context mismatch"));
        }
        if envelope.sender_ed != peer_ed {
            contacts::note_presented_ed(&tx, &c.contact_id, &envelope.sender_ed)?;
            tx.commit()
                .map_err(|_| OlmError::Store("call warning commit failed".into()))?;
            return Err(OlmError::IdentityMismatch);
        }
        // Preserve both crossed ratchets for older queued message ciphertext;
        // the same deterministic primary is used by ordinary TEXT and VOICE.
        sessions.sort_by_key(|s| s.session_id());
        let pickle = Zeroizing::new(olm::pickle_sessions(&sessions)?);
        if consumed_otk {
            let pickle = Zeroizing::new(
                serde_json::to_string(&account.pickle())
                    .map_err(|_| OlmError::Store("call account pickle failed".into()))?,
            );
            tx.execute(
                "UPDATE core_olm SET pickle=dmsg_seal('olm_pickle',?1) WHERE id=1",
                [pickle.as_str()],
            )
            .map_err(|_| OlmError::Store("call account write failed".into()))?;
        }
        tx.execute(
            "INSERT INTO core_sessions(contact_id,pickle,peer_ed,peer_curve)
             VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4)
             ON CONFLICT(contact_id) DO UPDATE SET pickle=excluded.pickle",
            params![
                c.contact_id,
                pickle.as_str(),
                peer_ed.as_slice(),
                peer_curve.as_slice()
            ],
        )
        .map_err(|_| OlmError::Store("call session write failed".into()))?;
        tx.commit()
            .map_err(|_| OlmError::Store("call decrypt commit failed".into()))?;
        self.account = account;
        self.next_key_id = next_key_id;
        Ok((c.contact_id, envelope))
    }
}

fn call_context(call_id: &[u8; 16], action: Action) -> Result<(), OlmError> {
    if *call_id == [0; 16] || !matches!(action, Action::Invite | Action::Accept) {
        return Err(OlmError::Protocol("invalid call identity/action"));
    }
    Ok(())
}

fn random_bytes<const N: usize>() -> Result<[u8; N], OlmError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| OlmError::Crypto("rng"))?;
    Ok(bytes)
}

// These loaders are call-specific and must only run inside the IMMEDIATE TX.
// Never substitute Core's cached account or a pre-network contact snapshot.
fn call_contact(conn: &Connection, contact_id: &str) -> Result<Contact, OlmError> {
    let c = contacts::get(conn, contact_id)?.ok_or(OlmError::UnknownContact)?;
    contacts::sendable(&c)?;
    // An outgoing contact invitation is sendable for chat bootstrap, but a
    // live call requires the completed accepted-contact state.
    if !matches!(
        c.state.as_str(),
        contacts::state::ACCEPTED | contacts::state::ACCEPTED_SERVER
    ) {
        return Err(OlmError::NotAccepted);
    }
    let (user, device, ed, curve) = contact_keys(&c)?;
    if user == [0; 16] || device == [0; 32] || ed == [0; 32] || curve == [0; 32] {
        return Err(OlmError::MissingKeys);
    }
    let (own_user, _) = crate::store::load_account(conn)
        .map_err(OlmError::Store)?
        .ok_or(OlmError::NotEnrolled)?;
    if user == own_user {
        return Err(OlmError::Protocol("self call"));
    }
    Ok(c)
}

fn call_identity(
    conn: &Connection,
    expected_device: &[u8; 32],
) -> Result<(Account, u32, [u8; 32]), OlmError> {
    let device = Zeroizing::new(
        crate::store::load_identity(conn)
            .map_err(OlmError::Store)?
            .ok_or(OlmError::NotEnrolled)?,
    );
    if olm::device_pubkey(&device) != *expected_device {
        return Err(OlmError::IdentityMismatch);
    }
    let (pickle, next) = crate::store::load_olm(conn)
        .map_err(OlmError::Store)?
        .ok_or(OlmError::NotEnrolled)?;
    let pickle = Zeroizing::new(pickle);
    let account = Account::from_pickle(
        serde_json::from_str(&pickle)
            .map_err(|_| OlmError::Store("call account pickle failed".into()))?,
    );
    let binding = crate::voice::own_binding(conn).map_err(OlmError::Store)?;
    Ok((account, next, binding))
}

fn pinned_sessions(conn: &Connection, c: &Contact) -> Result<Option<Vec<Session>>, OlmError> {
    let Some((pickle, ed, curve)) =
        crate::store::load_session(conn, &c.contact_id).map_err(OlmError::Store)?
    else {
        return Ok(None);
    };
    let (_, _, peer_ed, peer_curve) = contact_keys(c)?;
    if ed != peer_ed || curve != peer_curve {
        return Err(OlmError::IdentityMismatch);
    }
    let pickle = Zeroizing::new(pickle);
    Ok(Some(olm::unpickle_sessions(&pickle)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::RawEvent;
    use std::path::PathBuf;

    const ALICE: &str = "ACCE00000001";
    const BOB: &str = "B0BB00000002";
    const STORE_KEY: [u8; 32] = [73; 32];

    struct Pair {
        a: Core,
        b: Core,
        dir: PathBuf,
    }

    impl Pair {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("dmsg-call-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let a = fixture(&dir.join("a.db"), [1; 16], ALICE);
            let b = fixture(&dir.join("b.db"), [2; 16], BOB);
            for (own, peer, cid) in [(&a, &b, BOB), (&b, &a, ALICE)] {
                let (user, peer_id) = peer.my_account().unwrap();
                let (ed, curve) = peer.identity_keys();
                let qr =
                    contacts::build_qr(&peer_id, &user, &peer.device_pub(), &ed, &curve).unwrap();
                own.add_contact_qr(&qr).unwrap();
                own.accept_contact(cid).unwrap();
            }
            Self { a, b, dir }
        }
    }

    impl Drop for Pair {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    fn fixture(path: &std::path::Path, user: [u8; 16], cid: &str) -> Core {
        let conn = crate::store::open_encrypted(path, &STORE_KEY).unwrap();
        let device = Zeroizing::new(random_bytes::<32>().unwrap());
        crate::store::save_identity(&conn, &device).unwrap();
        crate::store::save_account(&conn, &user, cid).unwrap();
        Core::from_conn(conn).unwrap()
    }

    fn prime(sender: &mut Core, recipient: &mut Core, cid: &str) {
        recipient.account.generate_one_time_keys(1);
        olm::persist(&recipient.conn, &recipient.account, recipient.next_key_id).unwrap();
        let ot = *recipient
            .account
            .one_time_keys()
            .values()
            .next()
            .unwrap()
            .as_bytes();
        let (ed, curve) = recipient.identity_keys();
        let session = olm::outbound(&sender.account, &curve, &ot).unwrap();
        crate::store::save_session(
            &sender.conn,
            cid,
            &olm::pickle_session(&session).unwrap(),
            &ed,
            &curve,
        )
        .unwrap();
    }

    fn session(core: &Core, cid: &str) -> Option<(String, [u8; 32], [u8; 32])> {
        crate::store::load_session(&core.conn, cid).unwrap()
    }

    fn contact_metadata(core: &Core, cid: &str) -> (Contact, i64, i64, Option<Vec<u8>>) {
        let (read, activity, alias) = core.conn.query_row(
            "SELECT read_cursor,local_activity_ms,local_alias FROM core_contacts WHERE contact_id=?1",
            [cid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        (
            contacts::get(&core.conn, cid).unwrap().unwrap(),
            read,
            activity,
            alias,
        )
    }

    fn assert_no_messages(core: &Core, cid: &str) {
        for table in ["core_messages", "core_blob_transfers", "core_blob_chunks"] {
            let rows: i64 = core
                .conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0, "unexpected persisted call content in {table}");
        }
        assert!(core.history_page(cid, None, 100).unwrap().rows.is_empty());
        assert!(crate::store::outbox_queued(&core.conn, 0, 100)
            .unwrap()
            .0
            .is_empty());
        assert!(crate::store::inbox_list(&core.conn, 0, 100)
            .unwrap()
            .0
            .is_empty());
    }

    fn receive(
        core: &mut Core,
        peer: &Core,
        wire: &[u8],
        id: [u8; 16],
        action: Action,
    ) -> KeyEnvelope {
        let (cid, envelope) = core
            .decrypt_call(
                peer.my_account().unwrap().0,
                peer.device_pub(),
                wire,
                id,
                action,
            )
            .unwrap();
        assert_eq!(cid, peer.my_account().unwrap().1);
        envelope
    }

    fn assert_same_offer(left: &KeyEnvelope, right: &KeyEnvelope) {
        // No Debug implementation or failing assertion that prints keys.
        let left = Zeroizing::new(left.encode().unwrap());
        let right = Zeroizing::new(right.encode().unwrap());
        assert!(left.as_slice() == right.as_slice(), "call envelope differs");
    }

    fn text_event(core: &Core, mid: [u8; 16], seq: u64) -> RawEvent {
        RawEvent {
            seq,
            sender: core.device_pub(),
            sender_user: core.my_account().unwrap().0,
            message_id: mid,
            ciphertext: core.outbox_ciphertext(&mid).unwrap(),
        }
    }

    #[tokio::test]
    async fn first_olm_invite_accept_is_ephemeral_and_following_text_survives_reopen() {
        let mut pair = Pair::new("first");
        let id = [11; 16];
        let a_metadata = contact_metadata(&pair.a, BOB);
        let b_metadata = contact_metadata(&pair.b, ALICE);
        let peer = pair.a.call_peer(BOB).unwrap();
        assert_eq!(peer.user, pair.b.my_account().unwrap().0);
        assert_eq!(peer.device, pair.b.device_pub());
        assert_eq!(
            peer.binding,
            crate::voice::own_binding(&pair.b.conn).unwrap()
        );
        let invite = pair.a.call_offer(BOB, id, Action::Invite).unwrap();
        assert_eq!(
            pair.a.encrypt_call(BOB, &invite),
            Err(OlmError::MessageUnavailable)
        );
        prime(&mut pair.a, &mut pair.b, BOB);
        let initial_pins = session(&pair.a, BOB).unwrap();
        let b_otks = pair.b.account.one_time_keys().len();
        let wire = pair.a.encrypt_call(BOB, &invite).unwrap();
        assert_eq!(wire[0], 0, "the very first Olm message is an Invite");
        let incoming = receive(&mut pair.b, &pair.a, &wire, id, Action::Invite);
        assert_same_offer(&invite, &incoming);
        assert_eq!(pair.b.account.one_time_keys().len(), b_otks - 1);
        let answer = pair.b.call_offer(ALICE, id, Action::Accept).unwrap();
        assert!(answer.key != invite.key);
        assert_ne!(answer.ssrc, invite.ssrc);
        assert_ne!(answer.cname, invite.cname);
        let answer_wire = pair.b.encrypt_call(ALICE, &answer).unwrap();
        assert_eq!(answer_wire[0], 1);
        let accepted = receive(&mut pair.a, &pair.b, &answer_wire, id, Action::Accept);
        assert_same_offer(&answer, &accepted);
        assert_eq!(contact_metadata(&pair.a, BOB), a_metadata);
        assert_eq!(contact_metadata(&pair.b, ALICE), b_metadata);
        let current_pins = session(&pair.a, BOB).unwrap();
        assert_eq!(
            (current_pins.1, current_pins.2),
            (initial_pins.1, initial_pins.2)
        );
        assert_no_messages(&pair.a, BOB);
        assert_no_messages(&pair.b, ALICE);
        let repeated = pair.a.call_offer(BOB, [12; 16], Action::Invite).unwrap();
        assert!(repeated.key != invite.key);
        assert_ne!(repeated.ssrc, invite.ssrc);
        assert_ne!(repeated.cname, invite.cname);
        pair.a = Core::open_encrypted(&pair.dir.join("a.db"), &STORE_KEY).unwrap();
        pair.b = Core::open_encrypted(&pair.dir.join("b.db"), &STORE_KEY).unwrap();
        assert_no_messages(&pair.a, BOB);
        assert_no_messages(&pair.b, ALICE);
        let mid = pair
            .a
            .queue_text_existing_session(BOB, "text after the call", None)
            .unwrap()
            .unwrap();
        let event = text_event(&pair.a, mid, 1);
        assert_eq!(event.ciphertext[0], 1);
        let received = pair.b.decrypt_event(&event).await.unwrap().unwrap();
        assert_eq!(received.text, "text after the call");
    }

    #[tokio::test]
    async fn crossed_call_and_text_keep_both_old_ratchets_and_choose_the_same_primary() {
        let mut pair = Pair::new("crossed");
        prime(&mut pair.a, &mut pair.b, BOB);
        prime(&mut pair.b, &mut pair.a, ALICE);
        let a_old = pair
            .a
            .queue_text_existing_session(BOB, "a queued before call", None)
            .unwrap()
            .unwrap();
        let b_old = pair
            .b
            .queue_text_existing_session(ALICE, "b queued before call", None)
            .unwrap()
            .unwrap();
        let a_event = text_event(&pair.a, a_old, 1);
        let b_event = text_event(&pair.b, b_old, 1);
        let invite = pair.a.call_offer(BOB, [21; 16], Action::Invite).unwrap();
        let wire = pair.a.encrypt_call(BOB, &invite).unwrap();
        let incoming = receive(&mut pair.b, &pair.a, &wire, [21; 16], Action::Invite);
        assert_same_offer(&invite, &incoming);
        // B can answer on either crossed session, depending on the shared sort.
        let answer = pair.b.call_offer(ALICE, [21; 16], Action::Accept).unwrap();
        let wire = pair.b.encrypt_call(ALICE, &answer).unwrap();
        receive(&mut pair.a, &pair.b, &wire, [21; 16], Action::Accept);
        assert_eq!(
            pair.a.decrypt_event(&b_event).await.unwrap().unwrap().text,
            "b queued before call"
        );
        assert_eq!(
            pair.b.decrypt_event(&a_event).await.unwrap().unwrap().text,
            "a queued before call"
        );
        let a_sessions = olm::unpickle_sessions(&session(&pair.a, BOB).unwrap().0).unwrap();
        let b_sessions = olm::unpickle_sessions(&session(&pair.b, ALICE).unwrap().0).unwrap();
        assert_eq!(a_sessions.len(), 2);
        assert_eq!(b_sessions.len(), 2);
        assert_eq!(a_sessions[0].session_id(), b_sessions[0].session_id());
        assert_eq!(a_sessions[1].session_id(), b_sessions[1].session_id());
        for (core, cid) in [(&pair.a, BOB), (&pair.b, ALICE)] {
            assert_eq!(core.history_page(cid, None, 100).unwrap().rows.len(), 2);
            let count: i64 = core
                .conn
                .query_row("SELECT count(*) FROM core_messages", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                count, 2,
                "only the old TEXTs may enter history/outbox/inbox"
            );
        }
    }

    // Encrypt malformed authenticated bodies without using the strict public
    // encrypt_call path, so receive validation exercises actual Olm messages.
    fn raw_call(core: &mut Core, cid: &str, plain: &[u8]) -> Vec<u8> {
        let tx = core
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let c = contacts::get(&tx, cid).unwrap().unwrap();
        let mut sessions = pinned_sessions(&tx, &c).unwrap().unwrap();
        let wire = olm::encode_wire(&sessions[0].encrypt(plain).unwrap());
        let (_, _, ed, curve) = contact_keys(&c).unwrap();
        crate::store::save_session(
            &tx,
            cid,
            &olm::pickle_sessions(&sessions).unwrap(),
            &ed,
            &curve,
        )
        .unwrap();
        tx.commit().unwrap();
        wire
    }

    #[test]
    fn rejected_context_body_and_integrity_leave_account_otks_and_ratchets_untouched() {
        let mut pair = Pair::new("rollback");
        prime(&mut pair.a, &mut pair.b, BOB);
        let id = [31; 16];
        let mut invite = pair.a.call_offer(BOB, id, Action::Invite).unwrap();
        let wire = pair.a.encrypt_call(BOB, &invite).unwrap();
        let mut bad_binding = Zeroizing::new(invite.encode().unwrap());
        bad_binding[92] ^= 1; // recipient binding in the first prekey session
        let bad_binding_wire = raw_call(&mut pair.a, BOB, &bad_binding);
        let a_user = pair.a.my_account().unwrap().0;
        let a_device = pair.a.device_pub();
        let before = crate::store::load_olm(&pair.b.conn).unwrap();
        let b_metadata = contact_metadata(&pair.b, ALICE);
        for (user, device, expected_id, expected_action, ct) in [
            (a_user, a_device, id, Action::Accept, wire.clone()),
            (a_user, a_device, id, Action::Invite, bad_binding_wire),
            (a_user, a_device, [32; 16], Action::Invite, wire.clone()),
            (a_user, [99; 32], id, Action::Invite, wire.clone()),
            ([99; 16], a_device, id, Action::Invite, wire.clone()),
            (a_user, a_device, id, Action::Invite, {
                let mut ct = wire.clone();
                *ct.last_mut().unwrap() ^= 1;
                ct
            }),
        ] {
            assert!(pair
                .b
                .decrypt_call(user, device, &ct, expected_id, expected_action)
                .is_err());
            assert!(session(&pair.b, ALICE).is_none());
            assert!(crate::store::load_olm(&pair.b.conn).unwrap() == before);
            assert_eq!(contact_metadata(&pair.b, ALICE), b_metadata);
        }
        // Failed first decrypts did not consume the one-time key.
        receive(&mut pair.b, &pair.a, &wire, id, Action::Invite);
        let answer = pair.b.call_offer(ALICE, id, Action::Accept).unwrap();
        let answer_wire = pair.b.encrypt_call(ALICE, &answer).unwrap();
        receive(&mut pair.a, &pair.b, &answer_wire, id, Action::Accept);
        let a_before = session(&pair.a, BOB).unwrap();
        let saved_binding = invite.recipient_binding;
        invite.recipient_binding = [99; 32];
        assert_eq!(
            pair.a.encrypt_call(BOB, &invite),
            Err(OlmError::IdentityMismatch)
        );
        assert!(session(&pair.a, BOB).unwrap() == a_before);
        invite.recipient_binding = saved_binding;
        invite.action = Action::End;
        assert!(pair.a.encrypt_call(BOB, &invite).is_err());
        assert!(session(&pair.a, BOB).unwrap() == a_before);
        invite.action = Action::Invite;
        let mut malformed = Vec::new();
        let mut bad = Zeroizing::new(invite.encode().unwrap());
        bad[60] ^= 1; // sender binding
        malformed.push(raw_call(&mut pair.a, BOB, &bad));
        let mut bad = Zeroizing::new(invite.encode().unwrap());
        bad[92] ^= 1; // recipient binding
        malformed.push(raw_call(&mut pair.a, BOB, &bad));
        let mut bad = Zeroizing::new(invite.encode().unwrap());
        bad[59] = 2; // epoch != 1
        malformed.push(raw_call(&mut pair.a, BOB, &bad));
        let mut bad = Zeroizing::new(invite.encode().unwrap());
        bad[1] = 1; // ordinary TEXT must not decode as CALL
        malformed.push(raw_call(&mut pair.a, BOB, &bad));
        let mut bad = Zeroizing::new(invite.encode().unwrap());
        *bad.last_mut().unwrap() = 2; // unsupported profile
        malformed.push(raw_call(&mut pair.a, BOB, &bad));
        let before = session(&pair.b, ALICE).unwrap();
        let account = crate::store::load_olm(&pair.b.conn).unwrap();
        for wire in &malformed {
            assert!(pair
                .b
                .decrypt_call(a_user, a_device, wire, id, Action::Invite)
                .is_err());
            assert!(session(&pair.b, ALICE).unwrap() == before);
            assert!(crate::store::load_olm(&pair.b.conn).unwrap() == account);
            assert_eq!(contact_metadata(&pair.b, ALICE), b_metadata);
        }
        let wire = pair.a.encrypt_call(BOB, &invite).unwrap();
        // CALL sent through the ordinary history parser must roll back too.
        let ordinary = RawEvent {
            seq: 1,
            sender: a_device,
            sender_user: a_user,
            message_id: id,
            ciphertext: wire.clone(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(pair.b.decrypt_event(&ordinary)).is_err());
        assert!(session(&pair.b, ALICE).unwrap() == before);
        receive(&mut pair.b, &pair.a, &wire, id, Action::Invite);
        let before = session(&pair.b, ALICE).unwrap();
        assert!(pair
            .b
            .decrypt_call(a_user, a_device, &wire, id, Action::Invite)
            .is_err());
        assert!(session(&pair.b, ALICE).unwrap() == before);
        assert_no_messages(&pair.a, BOB);
        assert_no_messages(&pair.b, ALICE);
    }

    #[tokio::test]
    async fn concurrent_text_and_call_handles_share_the_persisted_sending_ratchet() {
        let mut pair = Pair::new("concurrent");
        prime(&mut pair.a, &mut pair.b, BOB);
        let path = pair.dir.join("a.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let text_barrier = barrier.clone();
        let text_path = path.clone();
        let text = std::thread::spawn(move || {
            let mut core = Core::open_encrypted(&text_path, &STORE_KEY).unwrap();
            text_barrier.wait();
            core.queue_text_existing_session(BOB, "concurrent ordinary text", None)
                .unwrap()
                .unwrap()
        });
        let call = std::thread::spawn(move || {
            let mut core = Core::open_encrypted(&path, &STORE_KEY).unwrap();
            let offer = core.call_offer(BOB, [41; 16], Action::Invite).unwrap();
            barrier.wait();
            core.encrypt_call(BOB, &offer).unwrap()
        });
        let mid = text.join().unwrap();
        let wire = call.join().unwrap();
        // Receive in reverse order if TEXT won the write lock, exercising Olm
        // skipped-message keys as well as fresh account/session reads.
        receive(&mut pair.b, &pair.a, &wire, [41; 16], Action::Invite);
        let event = text_event(&pair.a, mid, 1);
        let received = pair.b.decrypt_event(&event).await.unwrap().unwrap();
        assert_eq!(received.text, "concurrent ordinary text");
        assert_eq!(pair.a.history_page(BOB, None, 100).unwrap().rows.len(), 1);
        assert_eq!(pair.b.history_page(ALICE, None, 100).unwrap().rows.len(), 1);
        let next = pair.a.call_offer(BOB, [42; 16], Action::Invite).unwrap();
        let wire = pair.a.encrypt_call(BOB, &next).unwrap();
        receive(&mut pair.b, &pair.a, &wire, [42; 16], Action::Invite);
    }
}
