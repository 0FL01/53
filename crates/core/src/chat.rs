//! K3 сессии 1-на-1 + outbox + приём.
//!
//! Инварианты (ARCH §7, план K3):
//! - ratchet и ciphertext-outbox — ОДНА TX: сессия читается после write-lock,
//!   шифруется локально, затем `core_sessions`-пикл + `core_messages`-событие
//!   и sealed effective text коммитятся вместе; при ошибке TX локальная копия отбрасывается
//!   (перешифровка — только несохранённого/нового; ретрай шлёт ТОЛЬКО
//!   сохранённый ciphertext с тем же message_id — сервер дедуплицирует);
//! - статусы outbox: queued → accepted (SEND_ACK ST_ACCEPTED) → delivered
//!   (SEND_ACK ST_DELIVERED при повторе после доставки);
//! - strict versioned TEXT/EDIT/DELETE inside Olm, including authenticated MID;
//!   принятие сверяет ed с пином (подмена → seen + пропуск, отправка СТОП);
//! - получение: FETCH → durable inbox dedup → decrypt → inbox INSERT OR IGNORE
//!   → DELIVERY_ACK всеми seq (cursor двигает сервер;
//!   ответ сервера — cursor u64, 8 байт — см. `ack_reply` в main.rs;
//!   это НЕ формат `parse_delivery_ack`, клиент парсит факт сервера).
//!
//! Refill-побудки: send-path и fetch-path зовут `ensure_prekeys`,
//! reconnect — [`Core::on_reconnect`], ответ claim — внутри send-path
//! (ensure идёт до claim, COUNT после — см. olm.rs).
//!
//! Every connection resumes using its Noise device key after WELCOME. Account
//! credentials/invitations are never needed for mailbox access. Each network
//! method resumes and compares AUTHENTICATED with its immutable local account.

use dmsg_protocol::{
    decode_frame, mailbox as mp, OP_AUTHENTICATED, OP_DELIVERY_ACK, OP_ERROR, OP_FETCH,
    OP_FETCH_RESP, OP_RESUME, OP_SEND, OP_SEND_ACK, ST_ACCEPTED, ST_DELIVERED, TEXT_MAX,
};

use crate::contacts::{self, Contact};
use crate::olm::{self, OlmError};
use crate::transport::Transport;

/// Сырое событие mailbox (без decrypt — для evidence и диагностики K4).
pub struct RawEvent {
    pub seq: u64,
    pub sender: [u8; 32],
    pub sender_user: [u8; 16],
    pub message_id: [u8; 16],
    pub ciphertext: Vec<u8>,
}

/// Расшифрованное входящее.
#[derive(Clone, PartialEq, Eq)]
pub struct Received {
    pub contact_id: String,
    pub text: String,
    pub message_id: [u8; 16],
    pub seq: u64,
}

impl std::fmt::Debug for Received {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Received")
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

/// Итог приёма одной пачки.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FetchResult {
    pub received: Vec<Received>,
    /// Пропущено: неизвестный отправитель (cursor всё равно двинут).
    pub skipped_unknown: usize,
    /// Пропущено: заблокированный контакт.
    pub skipped_blocked: usize,
    /// Пропущено: не расшифровалось (битые байты, нет сессии, replay).
    pub skipped_undecryptable: usize,
    /// Пропущено: подмена identity (seen выставлен, отправка СТОП).
    pub skipped_mismatch: usize,
    /// Cursor сервера после DELIVERY_ACK.
    pub cursor: u64,
}

/// Статистика ретрая outbox.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RetryStats {
    pub resent: usize,
    pub accepted: usize,
    pub delivered: usize,
    pub skipped: usize,
}

/// Core: SQLite connection, Olm account and stable device identity.
/// Одно на процесс (как Supervisor); `Connection` не Sync — не шарить.
pub struct Core {
    conn: rusqlite::Connection,
    account: vodozemac::olm::Account,
    next_key_id: u32,
    device_pub: [u8; 32],
}

impl Core {
    /// Открыть ядро: store + Account (создаётся при первом запуске) +
    /// durable device key and accepted account; absent account is NotEnrolled.
    pub fn open(db_path: &std::path::Path) -> Result<Self, OlmError> {
        let conn = crate::store::open(db_path).map_err(OlmError::Store)?;
        Self::from_conn(conn)
    }

    /// Same core, with an authenticated encrypted local store.
    pub fn open_encrypted(db_path: &std::path::Path, key: &[u8]) -> Result<Self, OlmError> {
        let conn = crate::store::open_encrypted(db_path, key).map_err(OlmError::Store)?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: rusqlite::Connection) -> Result<Self, OlmError> {
        crate::store::load_account(&conn)
            .map_err(OlmError::Store)?
            .ok_or(OlmError::NotEnrolled)?;
        let device_priv = crate::store::load_identity(&conn)
            .map_err(OlmError::Store)?
            .ok_or(OlmError::NotEnrolled)?;
        let (account, next_key_id) = olm::load_or_create(&conn)?;
        let device_pub = olm::device_pubkey(&device_priv);
        Ok(Self {
            conn,
            account,
            next_key_id,
            device_pub,
        })
    }

    /// Свои Olm identity-ключи (ed, curve) — для contact-QR.
    pub fn identity_keys(&self) -> ([u8; 32], [u8; 32]) {
        (
            olm::ed_identity(&self.account),
            olm::curve_identity(&self.account),
        )
    }

    /// Свой user_id/contact_id из AUTHENTICATED (для QR и диагностики).
    pub fn my_account(&self) -> Result<([u8; 16], String), OlmError> {
        crate::store::load_account(&self.conn)
            .map_err(OlmError::Store)?
            .ok_or(OlmError::NotEnrolled)
    }

    /// Свой Noise static-публичник (device_key для QR).
    pub fn device_pub(&self) -> [u8; 32] {
        self.device_pub
    }

    /// Сканировать contact-QR (pin ключей / фиксация подмены в seen).
    pub fn add_contact_qr(&self, uri: &str) -> Result<contacts::QrResult, OlmError> {
        contacts::add_from_qr(&self.conn, uri)
    }

    /// Запрос на добавление по ID (без ключей).
    pub fn request_contact(&self, contact_id: &str) -> Result<String, OlmError> {
        contacts::request_add(&self.conn, contact_id)
    }

    /// Согласие на контакт.
    pub fn accept_contact(&self, contact_id: &str) -> Result<(), OlmError> {
        contacts::accept(&self.conn, contact_id)
    }

    /// Блок контакта (терминален в v1).
    pub fn block_contact(&self, contact_id: &str) -> Result<(), OlmError> {
        contacts::block(&self.conn, contact_id)
    }

    /// Explicit confirmation pins the seen binding and deletes the old session.
    pub fn confirm_contact(&mut self, contact_id: &str) -> Result<(), OlmError> {
        contacts::confirm_identity(&self.conn, contact_id)?;
        Ok(())
    }

    pub fn history_page(
        &self,
        contact_id: &str,
        before_local_id: Option<i64>,
        limit: u32,
    ) -> Result<crate::history::HistoryPage, crate::history::HistoryError> {
        crate::history::history_page(&self.conn, contact_id, before_local_id, limit)
    }

    pub fn message_status(
        &self,
        message_id_hex: &str,
    ) -> Result<Option<crate::history::DeliveryState>, crate::history::HistoryError> {
        crate::history::message_status(&self.conn, message_id_hex)
    }

    pub fn dialogs_page(
        &self,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<crate::history::DialogsPage, crate::history::HistoryError> {
        crate::history::dialogs_page(&self.conn, cursor, limit)
    }

    pub fn set_contact_alias(
        &self,
        contact_id: &str,
        alias: Option<&str>,
    ) -> Result<(), crate::history::HistoryError> {
        crate::history::set_contact_alias(&self.conn, contact_id, alias)
    }

    pub fn mark_read(
        &self,
        contact_id: &str,
        through_local_id: i64,
    ) -> Result<i64, crate::history::HistoryError> {
        crate::history::mark_read(&self.conn, contact_id, through_local_id)
    }

    /// Reconnect refill: key-only RESUME + COUNT + upload when needed.
    /// Зовёт шелл после переподключения (планировщика нет).
    pub async fn on_reconnect(&mut self, t: &mut impl Transport) -> Result<u32, OlmError> {
        self.login(t).await?;
        let count = olm::ensure_prekeys(
            &self.conn,
            &mut self.account,
            t,
            &self.device_pub,
            &mut self.next_key_id,
        )
        .await?;
        self.push_contact_requests(t).await?;
        Ok(count)
    }

    /// Same authenticated stream/poll as text; bounded durable request retry.
    async fn push_contact_requests(&mut self, t: &mut impl Transport) -> Result<(), OlmError> {
        use dmsg_protocol::{OP_CONTACT_OK, OP_CONTACT_REQUEST};
        let ids: Vec<String> = {
            let mut stmt=self.conn.prepare("SELECT contact_id FROM core_contacts WHERE state='inviting' ORDER BY contact_id LIMIT 32").map_err(|_|OlmError::Store("invite list".into()))?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|_| OlmError::Store("invite list".into()))?;
            rows.collect::<Result<_, _>>()
                .map_err(|_| OlmError::Store("invite list".into()))?
        };
        for id in ids {
            let c = contacts::get(&self.conn, &id)?.ok_or(OlmError::UnknownContact)?;
            if contacts::sendable(&c).is_err() {
                continue;
            }
            match self.refresh_binding(t, &c).await {
                Ok(()) => (),
                Err(OlmError::IdentityMismatch) => continue,
                // Requests contain only routing/public metadata. A fresh peer
                // may show its QR before publishing prekeys; SEND still has
                // the strict binding/claim gates before any text persistence.
                Err(OlmError::NoPeerPrekeys) => (),
                Err(e) => return Err(e),
            }
            let user = c.user_id.ok_or(OlmError::MissingKeys)?;
            t.send_frame(OP_CONTACT_REQUEST, &user)
                .await
                .map_err(|e| OlmError::Transport(e.to_string()))?;
            self.contact_reply(t, OP_CONTACT_OK).await?;
            self.conn.execute("UPDATE core_contacts SET state='accepted' WHERE contact_id=?1 AND state='inviting'",[&id]).map_err(|_|OlmError::Store("invite ack".into()))?;
        }
        Ok(())
    }

    async fn sync_contacts(&mut self, t: &mut impl Transport) -> Result<(), OlmError> {
        use dmsg_protocol::{
            OP_CONTACT_DECIDE, OP_CONTACT_OK, OP_CONTACT_REQUESTS, OP_CONTACT_REQUESTS_RESP,
        };
        self.push_contact_requests(t).await?;
        t.send_frame(OP_CONTACT_REQUESTS, &[])
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let payload = self.contact_reply(t, OP_CONTACT_REQUESTS_RESP).await?;
        let requests = dmsg_protocol::contacts::parse_requests(&payload)
            .ok_or(OlmError::Protocol("bad contact requests"))?;
        for profile in requests {
            let c = match contacts::receive_request(&self.conn, &profile) {
                Ok(c) => c,
                Err(OlmError::IdentityMismatch) => continue,
                Err(e) => return Err(e),
            };
            let decision = if c.state == contacts::state::BLOCKED {
                2
            } else if contacts::sendable(&c).is_ok() {
                1
            } else {
                continue;
            };
            let mut p = profile.binding.user_id.to_vec();
            p.push(decision);
            t.send_frame(OP_CONTACT_DECIDE, &p)
                .await
                .map_err(|e| OlmError::Transport(e.to_string()))?;
            self.contact_reply(t, OP_CONTACT_OK).await?;
        }
        Ok(())
    }

    async fn contact_reply(
        &self,
        t: &mut impl Transport,
        expected: u8,
    ) -> Result<Vec<u8>, OlmError> {
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_ERROR && p.len() == 1 {
            return Err(map_error(p[0]));
        }
        if op != expected || (expected == dmsg_protocol::OP_CONTACT_OK && !p.is_empty()) {
            return Err(OlmError::Protocol("bad contact reply"));
        }
        Ok(p)
    }

    async fn message_orders(
        &self,
        t: &mut impl Transport,
        keys: &[dmsg_protocol::chronology::Key],
    ) -> Result<Vec<Option<dmsg_protocol::chronology::Order>>, OlmError> {
        let p = dmsg_protocol::chronology::build_keys(keys)
            .ok_or(OlmError::Protocol("metadata keys"))?;
        t.send_frame(dmsg_protocol::OP_MESSAGE_METADATA, &p)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let p = self
            .contact_reply(t, dmsg_protocol::OP_MESSAGE_METADATA_RESP)
            .await?;
        dmsg_protocol::chronology::parse_orders(&p, keys.len())
            .ok_or(OlmError::Protocol("metadata response"))
    }

    /// Resume this connection using the Noise identity, without credentials.
    /// A foreign AUTHENTICATED reply is rejected, never attached or persisted.
    pub async fn login(&mut self, t: &mut impl Transport) -> Result<(), OlmError> {
        t.send_frame(OP_RESUME, &[])
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_AUTHENTICATED {
            let accepted = dmsg_protocol::auth::parse_authenticated(&p)
                .map_err(|_| OlmError::Protocol("bad authenticated response"))?;
            let (my_uid, my_cid) = self.my_account()?;
            if accepted.user_id != my_uid || accepted.contact_id != my_cid {
                return Err(OlmError::Protocol("authenticated account mismatch"));
            }
            return Ok(());
        }
        if op == OP_ERROR && p.len() == 1 {
            return Err(OlmError::Auth(crate::auth::map_error(p[0], false)));
        }
        Err(OlmError::Protocol("unexpected resume response"))
    }

    /// Отправить текст контакту. Возвращает message_id (16).
    /// Шаги: гейт sendable (до сети) → ensure_prekeys → сессия
    /// (claim при нужды) → encrypt → ОДНА TX (сессия + outbox) → SEND.
    pub async fn send_text(
        &mut self,
        t: &mut impl Transport,
        contact_id: &str,
        text: &str,
    ) -> Result<[u8; 16], OlmError> {
        self.preflight_text(contact_id, text)?;
        let c = contacts::get(&self.conn, contact_id)?.ok_or(OlmError::UnknownContact)?;
        let (_, device_key, peer_ed, peer_curve) = contact_keys(&c)?;
        self.login(t).await?;
        self.refresh_binding(t, &c).await?;
        olm::ensure_prekeys(
            &self.conn,
            &mut self.account,
            t,
            &self.device_pub,
            &mut self.next_key_id,
        )
        .await?;
        self.session_for(&c, &peer_ed, &peer_curve, t, &device_key)
            .await?;
        let (message_id, user_id, wire) = self
            .persist_text(contact_id, text)?
            .ok_or(OlmError::Protocol("session disappeared"))?;
        self.send_stored(t, &user_id, &message_id, &wire).await?;
        Ok(message_id)
    }

    /// Проверить локальные гейты до попытки подключиться: подмена не должна
    /// превращаться в успешную офлайн-очередь при сетевом отказе.
    pub(crate) fn preflight_text(&self, contact_id: &str, text: &str) -> Result<(), OlmError> {
        if text.is_empty() || text.len() > TEXT_MAX {
            return Err(OlmError::BadText);
        }
        let c = contacts::get(&self.conn, contact_id)?.ok_or(OlmError::UnknownContact)?;
        contacts::sendable(&c)?;
        let (_, _, peer_ed, peer_curve) = contact_keys(&c)?;
        if let Some((_, row_ed, row_curve)) =
            crate::store::load_session(&self.conn, contact_id).map_err(OlmError::Store)?
        {
            if row_ed != peer_ed || row_curve != peer_curve {
                return Err(OlmError::IdentityMismatch);
            }
        }
        Ok(())
    }

    /// Офлайн: только ранее сохранённая Olm-сессия. None означает отсутствие
    /// сессии — первый send требует сетевого CLAIM и не сохраняет plaintext.
    pub fn queue_text_existing_session(
        &mut self,
        contact_id: &str,
        text: &str,
    ) -> Result<Option<[u8; 16]>, OlmError> {
        self.preflight_text(contact_id, text)?;
        Ok(self.persist_text(contact_id, text)?.map(|(mid, _, _)| mid))
    }

    /// Одна и та же Olm-операция для online SEND и offline queue. IMMEDIATE
    /// сериализует конкурирующие Core/FFI handles ДО чтения пикла; stale кэш
    /// никогда не перезаписывает ratchet более позднего отправителя.
    fn persist_text(
        &mut self,
        contact_id: &str,
        text: &str,
    ) -> Result<Option<([u8; 16], [u8; 16], Vec<u8>)>, OlmError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| OlmError::Store(format!("tx: {e}")))?;
        let c = contacts::get(&tx, contact_id)?.ok_or(OlmError::UnknownContact)?;
        contacts::sendable(&c)?;
        let (user_id, _, peer_ed, peer_curve) = contact_keys(&c)?;
        let Some((pickle, row_ed, row_curve)) =
            crate::store::load_session(&tx, contact_id).map_err(OlmError::Store)?
        else {
            return Ok(None);
        };
        if row_ed != peer_ed || row_curve != peer_curve {
            return Err(OlmError::IdentityMismatch);
        }
        let mut sessions = olm::unpickle_sessions(&pickle)?;
        let session = &mut sessions[0];
        let mut message_id = [0u8; 16];
        getrandom::fill(&mut message_id).map_err(|_| OlmError::Crypto("rng"))?;
        let event = dmsg_protocol::e2e::Event {
            message_id,
            sender_ed: olm::ed_identity(&self.account),
            body: dmsg_protocol::e2e::Body::Text(text.into()),
        };
        let plain = dmsg_protocol::e2e::encode(&event).map_err(OlmError::Protocol)?;
        let msg = session
            .encrypt(&plain)
            .map_err(|_| OlmError::Crypto("encrypt"))?;
        let wire = olm::encode_wire(&msg);
        if wire.len() > dmsg_protocol::CIPHERTEXT_MAX {
            return Err(OlmError::Protocol("ciphertext too long"));
        }
        let spickle = olm::pickle_sessions(&sessions)?;
        tx.execute(
            "UPDATE core_sessions SET pickle=dmsg_seal('session_pickle',?2)
             WHERE contact_id=?1",
            rusqlite::params![contact_id, spickle],
        )
        .map_err(|e| OlmError::Store(format!("session: {e}")))?;
        crate::store::insert_outgoing(
            &tx,
            contact_id,
            &self.device_pub,
            &recipient_binding(&c)?,
            &event,
            &wire,
        )
        .map_err(OlmError::Store)?;
        tx.commit()
            .map_err(|e| OlmError::Store(format!("commit: {e}")))?;
        Ok(Some((message_id, user_id, wire)))
    }

    /// Local durable mutation only. All prerequisites and CAS are checked under
    /// the write lock; no network/CLAIM and no fallible reads after commit.
    pub fn edit_message(
        &mut self,
        contact_id: &str,
        local_id: i64,
        expected_revision: u64,
        text: &str,
    ) -> Result<crate::history::HistoryMessage, OlmError> {
        if text.is_empty() || text.len() > TEXT_MAX {
            return Err(OlmError::BadText);
        }
        self.mutate_message(
            contact_id,
            local_id,
            Some((expected_revision, text)),
            crate::history::DeleteScope::Everyone,
        )
    }

    pub fn delete_message(
        &mut self,
        contact_id: &str,
        local_id: i64,
        scope: crate::history::DeleteScope,
    ) -> Result<crate::history::HistoryMessage, OlmError> {
        self.mutate_message(contact_id, local_id, None, scope)
    }

    fn mutate_message(
        &mut self,
        contact_id: &str,
        local_id: i64,
        edit: Option<(u64, &str)>,
        scope: crate::history::DeleteScope,
    ) -> Result<crate::history::HistoryMessage, OlmError> {
        use crate::history::{DeleteScope, HistoryError, MessageDirection};
        use dmsg_protocol::e2e::{Body, Event};
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("mutation transaction failed".into()))?;
        let row = crate::history::message_row(&tx, contact_id, local_id).map_err(|e| match e {
            HistoryError::Store => OlmError::Store("mutation target read failed".into()),
            _ => OlmError::MessageUnavailable,
        })?;
        if row.direction != MessageDirection::Outgoing {
            return Err(OlmError::MessageUnavailable);
        }
        let (mid, sender): (Vec<u8>, Vec<u8>) = tx
            .query_row(
                "SELECT message_id,sender_device FROM core_messages WHERE local_id=?1",
                [local_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| OlmError::Store("mutation MID lookup failed".into()))?;
        if sender.as_slice() != self.device_pub {
            return Err(OlmError::MessageUnavailable);
        }
        let mid: [u8; 16] = mid
            .try_into()
            .map_err(|_| OlmError::Store("mutation MID corrupt".into()))?;
        if edit.is_none() && scope == DeleteScope::SelfOnly {
            tx.execute(
                "UPDATE core_messages SET hidden_self=1,text=NULL WHERE local_id=?1",
                [local_id],
            )
            .map_err(|_| OlmError::Store("self hide failed".into()))?;
            crate::store::clear_controls(&tx, contact_id, &self.device_pub, &mid)
                .map_err(OlmError::Store)?;
        } else {
            if row.deleted_all && edit.is_none() {
                tx.commit()
                    .map_err(|_| OlmError::Store("mutation commit failed".into()))?;
                return Ok(row);
            }
            if row.hidden_self
                || row.deleted_all
                || row.delivery_state == Some(crate::history::DeliveryState::Queued)
            {
                return Err(OlmError::MessageUnavailable);
            }
            if edit.is_some_and(|(expected, _)| expected != row.revision) {
                return Err(OlmError::MessageChanged);
            }
            let c = contacts::get(&tx, contact_id)?.ok_or(OlmError::UnknownContact)?;
            contacts::sendable(&c)?;
            let (_, _, peer_ed, peer_curve) = contact_keys(&c)?;
            let binding = recipient_binding(&c)?;
            if crate::store::outgoing_binding(&tx, &mid)
                .map_err(OlmError::Store)?
                .1
                != binding
            {
                return Err(OlmError::MessageUnavailable);
            }
            let Some((pickle, ed, curve)) =
                crate::store::load_session(&tx, contact_id).map_err(OlmError::Store)?
            else {
                return Err(OlmError::MessageUnavailable);
            };
            if ed != peer_ed || curve != peer_curve {
                return Err(OlmError::MessageUnavailable);
            }
            if edit.is_some_and(|(_, text)| text == row.text) {
                tx.commit()
                    .map_err(|_| OlmError::Store("mutation commit failed".into()))?;
                return Ok(row);
            }
            let revision = row
                .revision
                .checked_add(1)
                .filter(|r| *r <= i64::MAX as u64)
                .ok_or(OlmError::MessageUnavailable)?;
            let mut control_mid = [0; 16];
            getrandom::fill(&mut control_mid).map_err(|_| OlmError::Crypto("rng"))?;
            let event = Event {
                message_id: control_mid,
                sender_ed: olm::ed_identity(&self.account),
                body: match edit {
                    Some((_, text)) => Body::Edit {
                        target: mid,
                        revision,
                        text: text.into(),
                    },
                    None => Body::Delete {
                        target: mid,
                        revision,
                    },
                },
            };
            let mut sessions = olm::unpickle_sessions(&pickle)?;
            let msg = sessions[0]
                .encrypt(dmsg_protocol::e2e::encode(&event).map_err(OlmError::Protocol)?)
                .map_err(|_| OlmError::Crypto("encrypt"))?;
            let wire = olm::encode_wire(&msg);
            if wire.len() > dmsg_protocol::CIPHERTEXT_MAX {
                return Err(OlmError::Protocol("ciphertext too long"));
            }
            crate::store::save_session(
                &tx,
                contact_id,
                &olm::pickle_sessions(&sessions)?,
                &peer_ed,
                &peer_curve,
            )
            .map_err(OlmError::Store)?;
            if let Some((_, text)) = edit {
                tx.execute("UPDATE core_messages SET text=dmsg_seal('message_text',?2),revision=?3 WHERE local_id=?1",rusqlite::params![local_id,text,revision]).map_err(|_|OlmError::Store("edit target update failed".into()))?;
            } else {
                tx.execute("UPDATE core_messages SET text=NULL,deleted_all=1,revision=?2 WHERE local_id=?1",rusqlite::params![local_id,revision]).map_err(|_|OlmError::Store("delete target update failed".into()))?;
            }
            crate::store::insert_outgoing(
                &tx,
                contact_id,
                &self.device_pub,
                &binding,
                &event,
                &wire,
            )
            .map_err(OlmError::Store)?;
        }
        let result = crate::history::message_row(&tx, contact_id, local_id)
            .map_err(|_| OlmError::Store("mutation projection read failed".into()))?;
        tx.commit()
            .map_err(|_| OlmError::Store("mutation commit failed".into()))?;
        Ok(result)
    }

    /// Ретрай недоставленного ТЕМ ЖЕ ciphertext (без перешифровки).
    /// Пачками по 32; транспортная ошибка прерывает (прогресс сохранён).
    pub async fn retry_queued(&mut self, t: &mut impl Transport) -> Result<RetryStats, OlmError> {
        let mut stats = RetryStats::default();
        self.login(t).await?;
        let mut cursor = 0i64;
        loop {
            let (rows, next) =
                crate::store::outbox_queued(&self.conn, cursor, 32).map_err(OlmError::Store)?;
            if rows.is_empty() {
                break;
            }
            for (_, mid, cid, ct, _) in &rows {
                let c = contacts::get(&self.conn, cid)?.ok_or(OlmError::UnknownContact)?;
                // Гейт и здесь: блок/подмена после постановки в очередь — не шлём.
                if contacts::sendable(&c).is_err() {
                    stats.skipped += 1;
                    continue;
                }
                let (user_id, _, _, _) = contact_keys(&c)?;
                let (_, frozen) =
                    crate::store::outgoing_binding(&self.conn, mid).map_err(OlmError::Store)?;
                if frozen != recipient_binding(&c)? {
                    stats.skipped += 1;
                    continue;
                }
                match self.refresh_binding(t, &c).await {
                    Ok(()) => (),
                    Err(OlmError::IdentityMismatch) => {
                        stats.skipped += 1;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
                match self.send_stored(t, &user_id, mid, ct).await {
                    Ok(()) => {
                        stats.resent += 1;
                        // Статус уже обновлён внутри send_stored; классифицируем
                        // точечным SELECT (дешёвый, без полного скана).
                        match status_of(&self.conn, mid)? {
                            s if s == crate::store::outbox_status::DELIVERED => {
                                stats.delivered += 1
                            }
                            _ => stats.accepted += 1,
                        }
                    }
                    Err(OlmError::Transport(_)) => return Err(OlmError::Transport("retry".into())),
                    Err(
                        OlmError::Quota
                        | OlmError::Busy
                        | OlmError::MessageUnavailable
                        | OlmError::IdentityMismatch
                        | OlmError::Blocked
                        | OlmError::NotAccepted,
                    ) => {
                        stats.skipped += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
            match next {
                Some(n) => cursor = n,
                None => break,
            }
        }
        Ok(stats)
    }

    /// Ciphertext сохранённой outbox-записи (evidence «сервер видит только
    /// ciphertext», диагностика K4). Ретрай шлёт именно эти байты.
    pub fn outbox_ciphertext(&self, message_id: &[u8; 16]) -> Result<Vec<u8>, OlmError> {
        self.conn
            .query_row(
                "SELECT ciphertext FROM core_messages WHERE message_id=?1 AND direction='outgoing'",
                [message_id.as_slice()],
                |r| r.get(0),
            )
            .map_err(|e| OlmError::Store(format!("outbox: {e}")))
    }

    /// Сырой FETCH без decrypt/ack (evidence, диагностика K4).
    pub async fn fetch_raw(&mut self, t: &mut impl Transport) -> Result<Vec<RawEvent>, OlmError> {
        t.send_frame(OP_FETCH, &[])
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_FETCH_RESP {
            let events = mp::parse_fetch_resp(&p).ok_or(OlmError::Protocol("bad fetch"))?;
            return events
                .into_iter()
                .map(|e| {
                    Ok(RawEvent {
                        seq: e.seq,
                        sender: e
                            .sender
                            .try_into()
                            .map_err(|_| OlmError::Protocol("bad sender"))?,
                        sender_user: e
                            .sender_user
                            .try_into()
                            .map_err(|_| OlmError::Protocol("bad sender user"))?,
                        message_id: e
                            .message_id
                            .try_into()
                            .map_err(|_| OlmError::Protocol("bad msgid"))?,
                        ciphertext: e.ciphertext.to_vec(),
                    })
                })
                .collect();
        }
        if op == OP_ERROR && p.len() == 1 {
            return Err(map_error(p[0]));
        }
        Err(OlmError::Protocol("unexpected fetch reply"))
    }

    /// DELIVERY_ACK seqs. Возвращает cursor сервера (u64, 8 байт — факт
    /// `ack_reply`; НЕ формат `parse_delivery_ack`).
    pub async fn ack(&mut self, t: &mut impl Transport, seqs: &[u64]) -> Result<u64, OlmError> {
        // A zero-count ACK is an existing, idempotent protocol request: it
        // observes the durable server cursor without acknowledging any event.
        let mut payload = Vec::with_capacity(2 + 8 * seqs.len());
        payload.extend_from_slice(&(seqs.len() as u16).to_be_bytes());
        for s in seqs {
            payload.extend_from_slice(&s.to_be_bytes());
        }
        t.send_frame(OP_DELIVERY_ACK, &payload)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_DELIVERY_ACK {
            return p
                .as_slice()
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| OlmError::Protocol("bad ack"));
        }
        if op == OP_ERROR && p.len() == 1 {
            return Err(map_error(p[0]));
        }
        Err(OlmError::Protocol("unexpected ack reply"))
    }

    /// Приём одной пачки: FETCH → decrypt → inbox → ACK.
    /// Повторная пачка (replay/reorder) дедуплицируется локально
    /// (inbox IGNORE) и серверным cursor.
    pub async fn fetch_and_decrypt(
        &mut self,
        t: &mut impl Transport,
    ) -> Result<FetchResult, OlmError> {
        self.login(t).await?;
        olm::ensure_prekeys(
            &self.conn,
            &mut self.account,
            t,
            &self.device_pub,
            &mut self.next_key_id,
        )
        .await?;
        self.sync_contacts(t).await?;
        let events = self.fetch_raw(t).await?;
        let mut orders = std::collections::HashMap::new();
        let mut unseen = Vec::with_capacity(events.len());
        for event in &events {
            if !crate::store::durable_event(&self.conn, &event.sender, &event.message_id)
                .map_err(OlmError::Store)?
            {
                unseen.push(event);
            }
        }
        for chunk in unseen.chunks(dmsg_protocol::chronology::MAX) {
            let keys: Vec<_> = chunk.iter().map(|e| (e.sender, e.message_id)).collect();
            for (e, order) in chunk.iter().zip(self.message_orders(t, &keys).await?) {
                orders.insert(e.seq, order);
            }
        }
        let mut res = FetchResult::default();
        let mut seqs: Vec<u64> = Vec::with_capacity(events.len());
        for e in &events {
            // Durable replay is safe to ACK before looking up a newer binding.
            let durable = crate::store::durable_event(&self.conn, &e.sender, &e.message_id)
                .map_err(OlmError::Store)?;
            if durable {
                seqs.push(e.seq);
                continue;
            }
            if let Some(c) = contacts::get_by_user(&self.conn, &e.sender_user)? {
                if c.state != contacts::state::BLOCKED {
                    match self.refresh_binding(t, &c).await {
                        Ok(()) => (),
                        Err(OlmError::IdentityMismatch) => {
                            res.skipped_mismatch += 1;
                            continue;
                        }
                        Err(err) => return Err(err),
                    }
                    if c.device_key != Some(e.sender) {
                        res.skipped_mismatch += 1;
                        continue;
                    }
                }
            }
            match self
                .decrypt_event_order(e, orders.get(&e.seq).copied().flatten())
                .await
            {
                Ok(Some(r)) => {
                    res.received.push(r);
                    seqs.push(e.seq);
                }
                Ok(None) => {
                    seqs.push(e.seq);
                }
                Err(Fail::Skip(EventSkip::Unknown)) => {
                    res.skipped_unknown += 1;
                }
                Err(Fail::Skip(EventSkip::Blocked)) => {
                    res.skipped_blocked += 1;
                    seqs.push(e.seq);
                }
                Err(Fail::Skip(EventSkip::Undecryptable)) => res.skipped_undecryptable += 1,
                Err(Fail::Skip(EventSkip::Mismatch)) => res.skipped_mismatch += 1,
                Err(Fail::Err(e)) => return Err(e),
            }
        }
        // A text and a later control can share one FETCH batch. Only expose the
        // final effective projection, never its superseded/deleted transient body.
        let snapshot = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Deferred,
        )
        .map_err(|_| OlmError::Store("receive report snapshot failed".into()))?;
        let mut effective = Vec::with_capacity(res.received.len());
        for mut received in res.received {
            let text: Option<String>=snapshot.query_row("SELECT CASE WHEN hidden_self=1 OR deleted_all=1 THEN NULL ELSE CAST(dmsg_unseal('message_text',text) AS TEXT) END FROM core_messages WHERE direction='incoming' AND kind='text' AND contact_id=?1 AND message_id=?2",rusqlite::params![received.contact_id,received.message_id.as_slice()],|r|r.get(0)).map_err(|_|OlmError::Store("receive report projection failed".into()))?;
            if let Some(text) = text {
                received.text = text;
                effective.push(received);
            }
        }
        res.received = effective;
        drop(snapshot);
        res.cursor = self.ack(t, &seqs).await?;
        Ok(res)
    }

    /// Расшифровать одно событие. Ok(None) — дедуп-повтор (уже лежит).
    /// Unknown/unaccepted and identity/integrity failures stay unacknowledged.
    /// Explicitly blocked senders follow the existing drop policy.
    /// Fail::Err — жёсткая ошибка (store), прерывает пачку.
    #[cfg(test)]
    async fn decrypt_event(&mut self, e: &RawEvent) -> Result<Option<Received>, Fail> {
        self.decrypt_event_order(
            e,
            Some(dmsg_protocol::chronology::Order {
                seq: i64::try_from(e.seq).unwrap(),
                timestamp_ms: 1000,
            }),
        )
        .await
    }
    async fn decrypt_event_order(
        &mut self,
        e: &RawEvent,
        order: Option<dmsg_protocol::chronology::Order>,
    ) -> Result<Option<Received>, Fail> {
        // At-least-once replay cannot be decrypted twice by an Olm ratchet.
        // Only a durable inbox row permits this early return; a lookup failure
        // aborts the batch before ACK, and unseen events still undergo checks.
        let received = crate::store::durable_event(&self.conn, &e.sender, &e.message_id)
            .map_err(OlmError::Store)?;
        if received {
            return Ok(None);
        }
        let c = match contacts::get_by_user(&self.conn, &e.sender_user)? {
            Some(c) => c,
            None => return Err(Fail::Skip(EventSkip::Unknown)),
        };
        if c.state == contacts::state::BLOCKED {
            return Err(Fail::Skip(EventSkip::Blocked));
        }
        if !contacts::accepted(&c) {
            // Consent is explicit. Keep ciphertext in the bounded server
            // mailbox without consuming OTKs/ratchets or reporting Delivered.
            return Err(Fail::Skip(EventSkip::Unknown));
        }
        if c.device_key != Some(e.sender)
            || c.seen_user.is_some()
            || c.seen_ed.is_some()
            || c.seen_curve.is_some()
            || c.seen_device.is_some()
        {
            return Err(Fail::Skip(EventSkip::Mismatch));
        }
        let peer_ed = c.ed_identity.ok_or(Fail::Skip(EventSkip::Undecryptable))?;
        let peer_curve = c
            .curve_identity
            .ok_or(Fail::Skip(EventSkip::Undecryptable))?;
        let msg =
            olm::decode_wire(&e.ciphertext).map_err(|_| Fail::Skip(EventSkip::Undecryptable))?;
        // Load after the write lock, and decrypt into disposable local copies.
        // Failed integrity/identity checks never advance any ratchet or OTK.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("receive transaction".into()))?;
        let current = contacts::get(&tx, &c.contact_id)?.ok_or(OlmError::UnknownContact)?;
        if current != c {
            return Err(Fail::Skip(EventSkip::Mismatch));
        }
        let already =
            crate::store::durable_event(&tx, &e.sender, &e.message_id).map_err(OlmError::Store)?;
        if already {
            return Ok(None);
        }
        let (mut account, next_key_id) = olm::load_or_create(&tx)?;
        let mut sessions = if let Some((pickle, ed, curve)) =
            crate::store::load_session(&tx, &c.contact_id).map_err(OlmError::Store)?
        {
            if ed != peer_ed || curve != peer_curve {
                return Err(Fail::Skip(EventSkip::Mismatch));
            }
            olm::unpickle_sessions(&pickle)?
        } else {
            vec![]
        };
        let plaintext: Vec<u8> = match msg {
            vodozemac::olm::OlmMessage::PreKey(ref pre) => {
                if let Some(index) = sessions
                    .iter()
                    .position(|s| s.session_id() == pre.session_id())
                {
                    // Never try a prekey from another session against the
                    // outgoing ratchet; crossed first sends are legitimate.
                    sessions[index]
                        .decrypt(&msg)
                        .map_err(|_| Fail::Skip(EventSkip::Undecryptable))?
                } else {
                    if sessions.len() >= 2 {
                        return Err(Fail::Skip(EventSkip::Undecryptable));
                    }
                    let presented = *pre.identity_key().as_bytes();
                    match olm::inbound(&mut account, &peer_curve, pre) {
                        Ok((s, _, pt)) => {
                            sessions.push(s);
                            pt
                        }
                        Err(OlmError::IdentityMismatch) => {
                            // Подмена с receive-path: фиксируем presented
                            // (только если реально отличается от пина).
                            if presented != peer_curve {
                                contacts::note_presented_curve(&tx, &c.contact_id, &presented)?;
                            }
                            tx.commit()
                                .map_err(|_| OlmError::Store("warning commit".into()))?;
                            return Err(Fail::Skip(EventSkip::Mismatch));
                        }
                        Err(e) => return Err(Fail::Err(e)),
                    }
                }
            }
            vodozemac::olm::OlmMessage::Normal(_) => {
                let mut decrypted = None;
                for s in &mut sessions {
                    // Authentication failures discard candidate state; they
                    // cannot advance any persisted ratchet or consumed OTK.
                    let mut candidate = vodozemac::olm::Session::from_pickle(s.pickle());
                    if let Ok(text) = candidate.decrypt(&msg) {
                        *s = candidate;
                        decrypted = Some(text);
                        break;
                    }
                }
                decrypted.ok_or(Fail::Skip(EventSkip::Undecryptable))?
            }
        };
        let event = dmsg_protocol::e2e::decode(&plaintext)
            .map_err(|_| Fail::Skip(EventSkip::Undecryptable))?;
        if event.message_id != e.message_id {
            return Err(Fail::Skip(EventSkip::Undecryptable));
        }
        if event.sender_ed != peer_ed {
            contacts::note_presented_ed(&tx, &c.contact_id, &event.sender_ed)?;
            tx.commit()
                .map_err(|_| OlmError::Store("warning commit".into()))?;
            return Err(Fail::Skip(EventSkip::Mismatch));
        }
        if event.kind() == dmsg_protocol::e2e::Kind::Text
            && !order.is_some_and(|o| {
                o.seq > 0 && o.timestamp_ms >= 0 && u64::try_from(o.seq).ok() == Some(e.seq)
            })
        {
            return Err(Fail::Skip(EventSkip::Undecryptable));
        }
        if !crate::store::valid_control_target(&tx, &c.contact_id, &e.sender, &event)
            .map_err(OlmError::Store)?
        {
            return Err(Fail::Skip(EventSkip::Undecryptable));
        }
        // ОДНА TX: account (one-time consumed) + session + inbox dedup + history.
        // Both peers choose the same sending session after crossed initiation,
        // while retaining the other ratchet for already queued ciphertext.
        sessions.sort_by_key(|s| s.session_id());
        let apickle = serde_json::to_string(&account.pickle())
            .map_err(|_| OlmError::Store("pickle".into()))?;
        let spickle = olm::pickle_sessions(&sessions)?;
        let inserted = (|| -> Result<Option<Received>, OlmError> {
            tx.execute(
                "INSERT INTO core_olm(id, pickle, next_key_id) VALUES(1,dmsg_seal('olm_pickle',?1),?2)
                 ON CONFLICT(id) DO UPDATE SET pickle=excluded.pickle",
                rusqlite::params![apickle, next_key_id],
            )
            .map_err(|e| OlmError::Store(format!("olm: {e}")))?;
            tx.execute(
                "INSERT INTO core_sessions(contact_id, pickle, peer_ed, peer_curve)
                  VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4)
                 ON CONFLICT(contact_id) DO UPDATE SET pickle=excluded.pickle",
                rusqlite::params![
                    c.contact_id,
                    spickle,
                    peer_ed.as_slice(),
                    peer_curve.as_slice()
                ],
            )
            .map_err(|e| OlmError::Store(format!("session: {e}")))?;
            let id = crate::store::receive_event(&tx, &c.contact_id, &e.sender, &event, order)
                .map_err(OlmError::Store)?;
            let received = if let Some(id) = id {
                let row = crate::history::message_row(&tx, &c.contact_id, id)
                    .map_err(|_| OlmError::Store("incoming projection read failed".into()))?;
                if row.hidden_self || row.deleted_all {
                    None
                } else {
                    Some(Received {
                        contact_id: c.contact_id.clone(),
                        text: row.text,
                        message_id: e.message_id,
                        seq: e.seq,
                    })
                }
            } else {
                None
            };
            tx.commit()
                .map_err(|e| OlmError::Store(format!("commit: {e}")))?;
            Ok(received)
        })();
        if inserted.is_ok() {
            self.account = account;
            self.next_key_id = next_key_id;
        }
        inserted.map_err(Fail::Err)
    }

    /// Query the bounded directory record without silently changing a trusted pin.
    async fn refresh_binding(&self, t: &mut impl Transport, c: &Contact) -> Result<(), OlmError> {
        let user = c.user_id.ok_or(OlmError::MissingKeys)?;
        t.send_frame(dmsg_protocol::OP_DEVICE_BINDING, &user)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == dmsg_protocol::OP_ERROR && p.len() == 1 {
            return Err(map_error(p[0]));
        }
        if op != dmsg_protocol::OP_DEVICE_BINDING_RESP {
            return Err(OlmError::Protocol("unexpected binding response"));
        }
        let binding = dmsg_protocol::auth::parse_binding(&p)
            .map_err(|_| OlmError::Protocol("invalid binding"))?;
        contacts::check_binding(&self.conn, &c.contact_id, &binding)
    }

    /// Persisted pinned session, or a verified claim followed by atomic insertion.
    async fn session_for(
        &mut self,
        c: &Contact,
        peer_ed: &[u8; 32],
        peer_curve: &[u8; 32],
        t: &mut impl Transport,
        peer_device: &[u8; 32],
    ) -> Result<(), OlmError> {
        if let Some((_, row_ed, row_curve)) =
            crate::store::load_session(&self.conn, &c.contact_id).map_err(OlmError::Store)?
        {
            if row_ed != *peer_ed || row_curve != *peer_curve {
                return Err(OlmError::IdentityMismatch);
            }
        } else {
            // Новая сессия: claim one-time пира (сервер consume'ит атомарно).
            // Подпись claimed-ключа проверена сервером при upload его пином;
            // клиентский binding-контроль — pin на receive-path (prekey
            // identity + ed внутри envelope). Расхождение = подмена → СТОП.
            let (_, ot) = olm::claim_key(t, peer_device).await?;
            let s = olm::outbound(&self.account, peer_curve, &ot)?;
            // Конкурирующий sender мог уже создать сессию, пока шёл CLAIM.
            // Не заменяем его ratchet; persist_text прочитает победивший пикл.
            let sp = olm::pickle_session(&s)?;
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|_| OlmError::Store("session transaction".into()))?;
            let current = contacts::get(&tx, &c.contact_id)?.ok_or(OlmError::UnknownContact)?;
            if current != *c {
                return Err(OlmError::IdentityMismatch);
            }
            contacts::sendable(&current)?;
            tx.execute(
                "INSERT INTO core_sessions(contact_id, pickle, peer_ed, peer_curve)
                 VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4)
                 ON CONFLICT(contact_id) DO NOTHING",
                rusqlite::params![c.contact_id, sp, peer_ed.as_slice(), peer_curve.as_slice()],
            )
            .map_err(|e| OlmError::Store(format!("session: {e}")))?;
            tx.commit()
                .map_err(|_| OlmError::Store("session commit".into()))?;
        }
        Ok(())
    }

    /// SEND сохранённого ciphertext; обновляет статус по SEND_ACK.
    async fn send_stored(
        &mut self,
        t: &mut impl Transport,
        user_id: &[u8; 16],
        message_id: &[u8; 16],
        wire: &[u8],
    ) -> Result<(), OlmError> {
        let kind = {
            let tx = rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Deferred,
            )
            .map_err(|_| OlmError::Store("retry binding snapshot failed".into()))?;
            let cid:String=tx.query_row("SELECT contact_id FROM core_messages WHERE direction='outgoing' AND message_id=?1",[message_id.as_slice()],|r|r.get(0)).map_err(|_|OlmError::Store("retry target missing".into()))?;
            let c = contacts::get(&tx, &cid)?.ok_or(OlmError::UnknownContact)?;
            contacts::sendable(&c)?;
            let (user, _, ed, curve) = contact_keys(&c)?;
            let (kind, frozen) =
                crate::store::outgoing_binding(&tx, message_id).map_err(OlmError::Store)?;
            if user != *user_id || frozen != recipient_binding(&c)? {
                return Err(OlmError::MessageUnavailable);
            }
            let Some((_, session_ed, session_curve)) =
                crate::store::load_session(&tx, &cid).map_err(OlmError::Store)?
            else {
                return Err(OlmError::MessageUnavailable);
            };
            if session_ed != ed || session_curve != curve {
                return Err(OlmError::MessageUnavailable);
            }
            kind
        };
        let mut payload = Vec::with_capacity(32 + wire.len());
        payload.extend_from_slice(user_id);
        payload.extend_from_slice(message_id);
        payload.extend_from_slice(wire);
        t.send_frame(OP_SEND, &payload)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t
            .recv_frame()
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_SEND_ACK {
            let (mid, st) = mp::parse_send_ack(&p).ok_or(OlmError::Protocol("bad send_ack"))?;
            if mid != message_id {
                return Err(OlmError::Protocol("send_ack mismatch"));
            }
            let status = match st {
                ST_ACCEPTED => crate::store::outbox_status::ACCEPTED,
                ST_DELIVERED => crate::store::outbox_status::DELIVERED,
                _ => return Err(OlmError::Protocol("bad send status")),
            };
            let order = if kind == "text" {
                Some(
                    self.message_orders(t, &[(self.device_pub, *message_id)])
                        .await?
                        .into_iter()
                        .next()
                        .flatten()
                        .ok_or(OlmError::Protocol("missing send metadata"))?,
                )
            } else {
                None
            };
            crate::store::outbox_set_status_order(&self.conn, message_id, status, order)
                .map_err(OlmError::Store)?;
            return Ok(());
        }
        if op == OP_ERROR && p.len() == 1 {
            return Err(map_error(p[0]));
        }
        Err(OlmError::Protocol("unexpected send reply"))
    }
}

/// Классификация пропусков receive-path (внутренняя; наружу — счётчики
/// FetchResult, чтобы evidence читалось числами, а не строками).
#[derive(Debug, PartialEq, Eq)]
enum EventSkip {
    Unknown,
    Blocked,
    Undecryptable,
    Mismatch,
}

/// Исход обработки события: пропуск со счётчиком или жёсткая ошибка.
#[derive(Debug)]
enum Fail {
    Skip(EventSkip),
    Err(OlmError),
}

impl From<OlmError> for Fail {
    fn from(e: OlmError) -> Self {
        Fail::Err(e)
    }
}

/// Ключи контакта для отправки (всё обязательно — иначе MissingKeys).
fn contact_keys(c: &Contact) -> Result<([u8; 16], [u8; 32], [u8; 32], [u8; 32]), OlmError> {
    match (c.user_id, c.device_key, c.ed_identity, c.curve_identity) {
        (Some(u), Some(d), Some(e), Some(cv)) => Ok((u, d, e, cv)),
        _ => Err(OlmError::MissingKeys),
    }
}

/// One compact epoch binding, fixed with ciphertext and never recomputed for
/// an old event after explicit contact-identity confirmation.
fn recipient_binding(c: &Contact) -> Result<[u8; 32], OlmError> {
    use sha2::{Digest, Sha256};
    let (user, device, ed, curve) = contact_keys(c)?;
    let mut hash = Sha256::new();
    hash.update(b"dmsg recipient binding v1\0");
    hash.update(user);
    hash.update(device);
    hash.update(ed);
    hash.update(curve);
    Ok(hash.finalize().into())
}

/// Точечный статус outbox (для классификации ретрая).
fn status_of(conn: &rusqlite::Connection, mid: &[u8; 16]) -> Result<String, OlmError> {
    conn.query_row(
        "SELECT delivery_state FROM core_messages WHERE message_id=?1 AND direction='outgoing'",
        [mid.as_slice()],
        |r| r.get(0),
    )
    .map_err(|e| OlmError::Store(format!("status: {e}")))
}

fn map_error(code: u8) -> OlmError {
    use dmsg_protocol::{ERR_BAD, ERR_BUSY, ERR_NO_PREKEY, ERR_QUOTA, ERR_REVOKED};
    match code {
        ERR_NO_PREKEY => OlmError::NoPeerPrekeys,
        ERR_QUOTA => OlmError::Quota,
        ERR_REVOKED => OlmError::Revoked,
        ERR_BAD => OlmError::Bad,
        ERR_BUSY => OlmError::Busy,
        other => OlmError::Server(other),
    }
}

/// Декодировать один app-кадр из буфера с явным маппингом версии.
/// (Транспортный путь маппит так же — см. `TransportError::Frame`.)
pub fn decode_app_frame(buf: &[u8]) -> Result<(u8, Vec<u8>), OlmError> {
    let (_, op, payload, _) = decode_frame(buf).map_err(olm::map_frame_err)?;
    Ok((op, payload.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Transport, TransportError};
    use dmsg_protocol::{OP_COUNT, OP_COUNT_RESP, OP_PREKEY};
    use std::collections::VecDeque;

    /// Скриптованный double транспорта: очередь ответов + лог отправленного.
    /// Респондер может быть функцией от запроса (для echo message_id).
    struct Fake {
        replies: VecDeque<(u8, Vec<u8>)>,
        echo_send_ack: bool,
        pub sent: Vec<(u8, Vec<u8>)>,
        orders: std::collections::HashMap<
            dmsg_protocol::chronology::Key,
            dmsg_protocol::chronology::Order,
        >,
    }

    impl Fake {
        fn new(replies: Vec<(u8, Vec<u8>)>) -> Self {
            Self {
                replies: replies.into(),
                echo_send_ack: false,
                sent: Vec::new(),
                orders: Default::default(),
            }
        }
        fn with_echo() -> Self {
            Self {
                replies: VecDeque::new(),
                echo_send_ack: true,
                sent: Vec::new(),
                orders: Default::default(),
            }
        }
    }

    impl Transport for Fake {
        async fn connect(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
        async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), TransportError> {
            self.sent.push((opcode, payload.to_vec()));
            if opcode == dmsg_protocol::OP_MESSAGE_METADATA {
                let keys = dmsg_protocol::chronology::parse_keys(payload).unwrap();
                let orders: Vec<_> = keys
                    .iter()
                    .map(|key| {
                        Some(*self.orders.entry(*key).or_insert_with(|| {
                            dmsg_protocol::chronology::Order {
                                seq: (u64::from_be_bytes(key.1[..8].try_into().unwrap())
                                    % 1_000_000
                                    + 1) as i64,
                                timestamp_ms: 1000,
                            }
                        }))
                    })
                    .collect();
                self.replies.push_front((
                    dmsg_protocol::OP_MESSAGE_METADATA_RESP,
                    dmsg_protocol::chronology::build_orders(&orders).unwrap(),
                ));
            }
            // Empty request list is the default directory state in these
            // ratchet-focused doubles. Contact scenarios script a real list.
            if opcode == dmsg_protocol::OP_CONTACT_REQUESTS
                && self
                    .replies
                    .front()
                    .is_none_or(|(op, _)| *op != dmsg_protocol::OP_CONTACT_REQUESTS_RESP)
            {
                self.replies
                    .push_front((dmsg_protocol::OP_CONTACT_REQUESTS_RESP, vec![0]));
            }
            if self.echo_send_ack && opcode == OP_SEND {
                // SEND_ACK с тем же message_id, ST_ACCEPTED.
                let mut p = Vec::with_capacity(17);
                p.extend_from_slice(&payload[16..32]);
                p.push(ST_ACCEPTED);
                self.replies.push_back((OP_SEND_ACK, p));
            }
            Ok(())
        }
        async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
            let reply = self.replies.pop_front().ok_or(TransportError::Closed)?;
            if reply.0 == OP_FETCH_RESP {
                if let Some(events) = mp::parse_fetch_resp(&reply.1) {
                    for e in events {
                        self.orders.insert(
                            (
                                e.sender.try_into().unwrap(),
                                e.message_id.try_into().unwrap(),
                            ),
                            dmsg_protocol::chronology::Order {
                                seq: e.seq as i64,
                                timestamp_ms: 1000,
                            },
                        );
                    }
                }
            }
            Ok(reply)
        }
        async fn close(&mut self) {}
        fn is_connected(&self) -> bool {
            true
        }
    }

    fn tmp_core(name: &str) -> (Core, std::path::PathBuf) {
        tmp_core_mode(name, None)
    }

    fn tmp_core_mode(name: &str, key: Option<&[u8; 32]>) -> (Core, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmsg-k3t-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let db = dir.join("core.db");
        let _ = std::fs::remove_file(&db);
        // Fresh authenticated fixture; never initialize plain then convert.
        let conn = match key {
            Some(k) => crate::store::open_encrypted(&db, k),
            None => crate::store::open(&db),
        }
        .expect("open");
        let privk = {
            let params: snow::params::NoiseParams =
                crate::transport::PATTERN.parse().expect("pattern");
            let kp = snow::Builder::new(params)
                .generate_keypair()
                .expect("keygen");
            let b: [u8; 32] = kp.private.as_slice().try_into().expect("len");
            b
        };
        crate::store::save_identity(&conn, &privk).expect("identity");
        crate::store::save_account(&conn, &[9u8; 16], "TESTC0NTACT1").expect("account");
        drop(conn);
        (
            match key {
                Some(k) => Core::open_encrypted(&db, k),
                None => Core::open(&db),
            }
            .expect("core"),
            dir,
        )
    }

    /// Связать два ядра контактами через настоящий QR-путь (без сети).
    fn link(a: &mut Core, b: &mut Core, aid: &str, bid: &str) {
        let (auid, _) = a.my_account().expect("a account");
        let (buid, _) = b.my_account().expect("b account");
        let (aed, acurve) = a.identity_keys();
        let (bed, bcurve) = b.identity_keys();
        let aqr = contacts::build_qr(aid, &auid, &a.device_pub(), &aed, &acurve).expect("aqr");
        let bqr = contacts::build_qr(bid, &buid, &b.device_pub(), &bed, &bcurve).expect("bqr");
        // A знает B под bid, B знает A под aid.
        assert_eq!(
            contacts::add_from_qr(&a.conn, &bqr),
            Ok(contacts::QrResult::Added)
        );
        assert_eq!(
            contacts::add_from_qr(&b.conn, &aqr),
            Ok(contacts::QrResult::Added)
        );
        contacts::accept(&a.conn, bid).expect("a accept");
        contacts::accept(&b.conn, aid).expect("b accept");
    }

    fn count_resp(n: u32) -> (u8, Vec<u8>) {
        (OP_COUNT_RESP, n.to_be_bytes().to_vec())
    }

    /// AUTHENTICATED reply for key resume.
    fn authenticated_resp() -> (u8, Vec<u8>) {
        let mut p = vec![9u8; 16];
        p.extend_from_slice(b"TESTC0NTACT1");
        (OP_AUTHENTICATED, p)
    }
    fn binding_resp(core: &Core) -> (u8, Vec<u8>) {
        let (user, _) = core.my_account().unwrap();
        let (ed, curve) = core.identity_keys();
        (
            dmsg_protocol::OP_DEVICE_BINDING_RESP,
            dmsg_protocol::auth::build_binding(&user, &core.device_pub(), &ed, &curve),
        )
    }

    fn action_pair(
        name: &str,
    ) -> (
        Core,
        Core,
        std::path::PathBuf,
        std::path::PathBuf,
        [u8; 16],
        Vec<u8>,
        i64,
    ) {
        let (mut a, da) = tmp_core_mode(&format!("{name}-a"), Some(&[17; 32]));
        let (mut b, db) = tmp_core_mode(&format!("{name}-b"), Some(&[18; 32]));
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        b.account.generate_one_time_keys(1);
        olm::persist(&b.conn, &b.account, b.next_key_id).unwrap();
        let ot = *b
            .account
            .one_time_keys()
            .values()
            .next()
            .unwrap()
            .as_bytes();
        let (ed, curve) = b.identity_keys();
        let session = olm::outbound(&a.account, &curve, &ot).unwrap();
        crate::store::save_session(
            &a.conn,
            "BOBB00000002",
            &olm::pickle_session(&session).unwrap(),
            &ed,
            &curve,
        )
        .unwrap();
        let mid = a
            .queue_text_existing_session("BOBB00000002", "original private text")
            .unwrap()
            .unwrap();
        let wire = a.outbox_ciphertext(&mid).unwrap();
        crate::store::outbox_set_status_order(
            &a.conn,
            &mid,
            "accepted",
            Some(dmsg_protocol::chronology::Order {
                seq: 1,
                timestamp_ms: 1000,
            }),
        )
        .unwrap();
        let id = a.history_page("BOBB00000002", None, 1).unwrap().rows[0].local_id;
        (a, b, da, db, mid, wire, id)
    }

    fn latest_control(a: &Core, seq: u64) -> RawEvent {
        let (mid,ct):(Vec<u8>,Vec<u8>)=a.conn.query_row("SELECT message_id,ciphertext FROM core_messages WHERE direction='outgoing' AND kind!='text' ORDER BY local_id DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        RawEvent {
            seq,
            sender: a.device_pub(),
            sender_user: a.my_account().unwrap().0,
            message_id: mid.try_into().unwrap(),
            ciphertext: ct,
        }
    }

    fn exact(core: &Core, cid: &str, id: i64) -> crate::history::HistoryMessage {
        crate::history::history_message(&core.conn, cid, id).unwrap()
    }

    #[tokio::test]
    async fn editing_is_owned_cas_atomic_in_place_and_self_hide_is_local_even_blocked() {
        use crate::history::{DeleteScope, DeliveryState, HistoryError};
        let (mut a, mut b, da, db, mid, wire, id) = action_pair("actions-owned");
        let original = exact(&a, "BOBB00000002", id);
        b.decrypt_event(&RawEvent {
            seq: 1,
            sender: a.device_pub(),
            sender_user: a.my_account().unwrap().0,
            message_id: mid,
            ciphertext: wire.clone(),
        })
        .await
        .unwrap();
        let incoming = b.history_page("ALICE0000001", None, 1).unwrap().rows[0].local_id;
        assert_eq!(
            b.edit_message("ALICE0000001", incoming, 0, "foreign"),
            Err(OlmError::MessageUnavailable)
        );
        assert_eq!(
            b.delete_message("ALICE0000001", incoming, DeleteScope::Everyone),
            Err(OlmError::MessageUnavailable)
        );
        assert_eq!(
            b.delete_message("ALICE0000001", incoming, DeleteScope::SelfOnly),
            Err(OlmError::MessageUnavailable)
        );
        let edited = a
            .edit_message("BOBB00000002", id, 0, "replacement private text")
            .unwrap();
        assert_eq!(
            (
                edited.local_id,
                &edited.message_id_hex,
                edited.local_timestamp_ms,
                edited.server_seq,
                edited.server_timestamp_ms
            ),
            (
                original.local_id,
                &original.message_id_hex,
                original.local_timestamp_ms,
                original.server_seq,
                original.server_timestamp_ms
            )
        );
        assert_eq!(edited.revision, 1);
        assert_eq!(edited.delivery_state, Some(DeliveryState::Accepted));
        assert_eq!(edited.change_delivery_state, Some(DeliveryState::Queued));
        assert_eq!(a.outbox_ciphertext(&mid).unwrap(), wire);
        assert_eq!(
            a.edit_message("BOBB00000002", id, 0, "replacement private text"),
            Err(OlmError::MessageChanged)
        );
        let count = crate::store::outbox_queued(&a.conn, 0, 100)
            .unwrap()
            .0
            .len();
        assert_eq!(
            a.edit_message("BOBB00000002", id, 1, "replacement private text")
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            crate::store::outbox_queued(&a.conn, 0, 100)
                .unwrap()
                .0
                .len(),
            count
        );
        let control = latest_control(&a, 2);
        assert!(b
            .decrypt_event_order(&control, None)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            exact(&b, "ALICE0000001", incoming).text,
            "replacement private text"
        );
        assert_eq!(crate::store::inbox_count(&b.conn).unwrap(), 1);
        assert_eq!(b.dialogs_page(None, 10).unwrap().rows[0].local_unread, 1);
        let queued = a
            .queue_text_existing_session("BOBB00000002", "saved queued text")
            .unwrap()
            .unwrap();
        let queued_row = a
            .history_page("BOBB00000002", None, 1)
            .unwrap()
            .rows
            .remove(0);
        let queued_wire = a.outbox_ciphertext(&queued).unwrap();
        assert_eq!(
            a.edit_message("BOBB00000002", queued_row.local_id, 0, "not eligible"),
            Err(OlmError::MessageUnavailable)
        );
        assert_eq!(
            a.delete_message("BOBB00000002", queued_row.local_id, DeleteScope::Everyone),
            Err(OlmError::MessageUnavailable)
        );
        assert!(
            a.delete_message("BOBB00000002", queued_row.local_id, DeleteScope::SelfOnly)
                .unwrap()
                .hidden_self
        );
        assert_eq!(a.outbox_ciphertext(&queued).unwrap(), queued_wire);
        assert!(crate::store::outbox_queued(&a.conn, 0, 100)
            .unwrap()
            .0
            .iter()
            .any(|(_, event, _, _, state)| *event == queued && state == "queued"));
        let count = crate::store::outbox_queued(&a.conn, 0, 100)
            .unwrap()
            .0
            .len();
        a.block_contact("BOBB00000002").unwrap();
        let hidden = a
            .delete_message("BOBB00000002", id, DeleteScope::SelfOnly)
            .unwrap();
        assert!(hidden.hidden_self && !hidden.deleted_all && hidden.text.is_empty());
        assert_eq!(hidden.revision, 1);
        assert_eq!(
            crate::store::outbox_queued(&a.conn, 0, 100)
                .unwrap()
                .0
                .len(),
            count
        );
        assert_eq!(
            a.history_page("BOBB00000002", None, 100)
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(
            a.mark_read("BOBB00000002", id),
            Err(HistoryError::InvalidInput)
        );
        assert!(
            crate::history::timeline_page(&a.conn, "BOBB00000002", Some(id), 1)
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(a.dialogs_page(None, 10).unwrap().rows[0].preview.is_none());
        assert_eq!(
            crate::store::inbox_list(&b.conn, 0, 10).unwrap().0[0].2,
            "replacement private text"
        );
        assert_eq!(a.outbox_ciphertext(&mid).unwrap(), wire);
        drop(a);
        drop(b);
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }

    #[tokio::test]
    async fn control_before_original_reordered_edits_mid_binding_and_terminal_delete_survive_reopen(
    ) {
        use crate::history::DeleteScope;
        let (mut a, mut b, da, db, mid, wire, id) = action_pair("actions-reordered");
        a.edit_message("BOBB00000002", id, 0, "edit one").unwrap();
        let first = latest_control(&a, 2);
        a.edit_message("BOBB00000002", id, 1, "edit two").unwrap();
        let second = latest_control(&a, 3);
        b.decrypt_event_order(&second, None).await.unwrap();
        let state = crate::store::load_session(&b.conn, "ALICE0000001").unwrap();
        let wrong_mid = RawEvent {
            seq: first.seq,
            sender: first.sender,
            sender_user: first.sender_user,
            message_id: [99; 16],
            ciphertext: first.ciphertext.clone(),
        };
        assert!(matches!(
            b.decrypt_event_order(&wrong_mid, None).await,
            Err(Fail::Skip(EventSkip::Undecryptable))
        ));
        assert_eq!(
            crate::store::load_session(&b.conn, "ALICE0000001").unwrap(),
            state
        );
        b.decrypt_event_order(&first, None).await.unwrap();
        assert_eq!(
            b.history_page("ALICE0000001", None, 100)
                .unwrap()
                .rows
                .len(),
            0
        );
        assert_eq!(
            b.conn
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM core_messages WHERE kind='edit' AND text IS NOT NULL",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(b.conn.query_row::<String,_,_>("SELECT CAST(dmsg_unseal('message_text',text) AS TEXT) FROM core_messages WHERE text IS NOT NULL",[],|r|r.get(0)).unwrap(),"edit two");
        let deleted = a
            .delete_message("BOBB00000002", id, DeleteScope::Everyone)
            .unwrap();
        assert_eq!(deleted.revision, 3);
        let deletion = latest_control(&a, 4);
        b.decrypt_event_order(&deletion, None).await.unwrap();
        assert_eq!(
            b.conn
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM core_messages WHERE text IS NOT NULL",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            0
        );
        assert!(b
            .decrypt_event(&RawEvent {
                seq: 1,
                sender: a.device_pub(),
                sender_user: a.my_account().unwrap().0,
                message_id: mid,
                ciphertext: wire
            })
            .await
            .unwrap()
            .is_none());
        let row = b
            .history_page("ALICE0000001", None, 100)
            .unwrap()
            .rows
            .remove(0);
        assert!(row.deleted_all && row.text.is_empty());
        assert_eq!(row.server_seq, Some(1));
        assert_eq!(row.server_timestamp_ms, Some(1000));
        assert_eq!(crate::store::inbox_count(&b.conn).unwrap(), 1);
        assert!(crate::store::inbox_list(&b.conn, 0, 100)
            .unwrap()
            .0
            .is_empty());
        assert_eq!(b.dialogs_page(None, 10).unwrap().rows[0].local_unread, 0);
        assert!(b.dialogs_page(None, 10).unwrap().rows[0].preview.is_none());
        assert_eq!(b.conn.query_row::<i64,_,_>("SELECT count(*) FROM core_messages WHERE kind!='text' AND (server_seq IS NOT NULL OR server_timestamp_ms IS NOT NULL)",[],|r|r.get(0)).unwrap(),0);
        drop(b);
        let mut b = Core::open_encrypted(&db.join("core.db"), &[18; 32]).unwrap();
        assert!(b.decrypt_event_order(&first, None).await.unwrap().is_none());
        assert!(b
            .decrypt_event_order(&deletion, None)
            .await
            .unwrap()
            .is_none());
        assert_eq!(exact(&b, "ALICE0000001", row.local_id), row);
        let count = crate::store::outbox_queued(&a.conn, 0, 100)
            .unwrap()
            .0
            .len();
        assert_eq!(
            a.delete_message("BOBB00000002", id, DeleteScope::Everyone)
                .unwrap()
                .revision,
            3
        );
        assert_eq!(
            crate::store::outbox_queued(&a.conn, 0, 100)
                .unwrap()
                .0
                .len(),
            count
        );
        drop(a);
        drop(b);
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }

    #[tokio::test]
    async fn mutation_rollback_byte_identical_control_retry_and_frozen_recipient_epoch() {
        let (mut a, b, da, db, mid, wire, id) = action_pair("actions-rollback");
        let original = exact(&a, "BOBB00000002", id);
        let state = crate::store::load_session(&a.conn, "BOBB00000002").unwrap();
        a.conn.execute_batch("CREATE TRIGGER reject_control AFTER INSERT ON core_messages WHEN NEW.kind!='text' BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        assert!(matches!(
            a.edit_message("BOBB00000002", id, 0, "must roll back"),
            Err(OlmError::Store(_))
        ));
        assert_eq!(exact(&a, "BOBB00000002", id), original);
        assert_eq!(
            crate::store::load_session(&a.conn, "BOBB00000002").unwrap(),
            state
        );
        assert_eq!(
            crate::store::outbox_queued(&a.conn, 0, 100)
                .unwrap()
                .0
                .len(),
            1
        );
        a.conn.execute_batch("DROP TRIGGER reject_control").unwrap();
        a.edit_message("BOBB00000002", id, 0, "durable edit")
            .unwrap();
        let control = latest_control(&a, 2);
        crate::store::outbox_set_status(&a.conn, &mid, "delivered").unwrap();
        drop(a);
        let mut a = Core::open_encrypted(&da.join("core.db"), &[17; 32]).unwrap();
        let mut t = Fake::new(vec![authenticated_resp(), binding_resp(&b)]);
        t.echo_send_ack = true;
        assert_eq!(a.retry_queued(&mut t).await.unwrap().resent, 1);
        assert_eq!(
            t.sent.iter().find(|(op, _)| *op == OP_SEND).unwrap().1[32..],
            control.ciphertext
        );
        assert!(!t
            .sent
            .iter()
            .any(|(op, _)| *op == dmsg_protocol::OP_MESSAGE_METADATA));
        assert_eq!(
            a.message_status(&crate::history::hex(&control.message_id))
                .unwrap(),
            Some(crate::history::DeliveryState::Accepted)
        );
        assert_eq!(a.outbox_ciphertext(&mid).unwrap(), wire);
        let (ed, curve) = b.identity_keys();
        let changed = contacts::build_qr(
            "BOBB00000002",
            &b.my_account().unwrap().0,
            &[42; 32],
            &ed,
            &curve,
        )
        .unwrap();
        assert_eq!(
            a.add_contact_qr(&changed).unwrap(),
            contacts::QrResult::IdentityChanged
        );
        a.confirm_contact("BOBB00000002").unwrap();
        // Even a cached session matching the new pins cannot retarget old ciphertext.
        crate::store::save_session(&a.conn, "BOBB00000002", &state.unwrap().0, &ed, &curve)
            .unwrap();
        let mut t = Fake::new(vec![authenticated_resp()]);
        let stats = a.retry_queued(&mut t).await.unwrap();
        assert_eq!((stats.skipped, stats.resent), (1, 0));
        assert!(!t.sent.iter().any(|(op, _)| *op == OP_SEND));
        assert_eq!(
            a.edit_message("BOBB00000002", id, 1, "wrong recipient"),
            Err(OlmError::MessageUnavailable)
        );
        assert_eq!(exact(&a, "BOBB00000002", id).text, "durable edit");
        drop(a);
        drop(b);
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }

    fn fetch_batch(events: &[&RawEvent]) -> Vec<u8> {
        let mut body = (events.len() as u16).to_be_bytes().to_vec();
        for event in events {
            body.extend_from_slice(&event.seq.to_be_bytes());
            body.extend_from_slice(&event.sender);
            body.extend_from_slice(&event.sender_user);
            body.extend_from_slice(&event.message_id);
            body.extend_from_slice(&(event.ciphertext.len() as u16).to_be_bytes());
            body.extend_from_slice(&event.ciphertext);
        }
        body
    }

    #[tokio::test]
    async fn receive_control_projection_rolls_back_without_ack_and_final_batch_report_never_leaks_old_text(
    ) {
        let (mut a, mut b, da, db, mid, wire, id) = action_pair("actions-receive-rollback");
        let original = RawEvent {
            seq: 1,
            sender: a.device_pub(),
            sender_user: a.my_account().unwrap().0,
            message_id: mid,
            ciphertext: wire,
        };
        b.decrypt_event(&original).await.unwrap();
        a.edit_message("BOBB00000002", id, 0, "new effective text")
            .unwrap();
        let edit = latest_control(&a, 2);
        let batch = fetch_batch(&[&edit]);
        let make = || {
            Fake::new(vec![
                authenticated_resp(),
                count_resp(16),
                (OP_FETCH_RESP, batch.clone()),
                binding_resp(&a),
                (OP_DELIVERY_ACK, 2u64.to_be_bytes().to_vec()),
            ])
        };
        let before = b.history_page("ALICE0000001", None, 100).unwrap();
        let crypto = crate::store::load_session(&b.conn, "ALICE0000001").unwrap();
        b.conn.execute_batch("CREATE TRIGGER reject_projection BEFORE UPDATE OF revision ON core_messages WHEN OLD.kind='text' BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        let mut t = make();
        assert!(matches!(
            b.fetch_and_decrypt(&mut t).await,
            Err(OlmError::Store(_))
        ));
        assert!(!t.sent.iter().any(|(op, _)| *op == OP_DELIVERY_ACK));
        assert_eq!(b.history_page("ALICE0000001", None, 100).unwrap(), before);
        assert_eq!(
            crate::store::load_session(&b.conn, "ALICE0000001").unwrap(),
            crypto
        );
        assert!(!crate::store::durable_event(&b.conn, &a.device_pub(), &edit.message_id).unwrap());
        b.conn
            .execute_batch("DROP TRIGGER reject_projection")
            .unwrap();
        assert!(b
            .fetch_and_decrypt(&mut make())
            .await
            .unwrap()
            .received
            .is_empty());
        assert_eq!(
            b.history_page("ALICE0000001", None, 100).unwrap().rows[0].text,
            "new effective text"
        );
        drop(a);
        drop(b);
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();

        for delete in [false, true] {
            let (mut a, mut b, da, db, mid, wire, id) = action_pair(if delete {
                "actions-final-delete"
            } else {
                "actions-final-edit"
            });
            let original = RawEvent {
                seq: 1,
                sender: a.device_pub(),
                sender_user: a.my_account().unwrap().0,
                message_id: mid,
                ciphertext: wire,
            };
            a.edit_message("BOBB00000002", id, 0, "final effective text")
                .unwrap();
            let edit = latest_control(&a, 2);
            let deletion = if delete {
                a.delete_message("BOBB00000002", id, crate::history::DeleteScope::Everyone)
                    .unwrap();
                Some(latest_control(&a, 3))
            } else {
                None
            };
            let mut events = vec![&original, &edit];
            if let Some(event) = deletion.as_ref() {
                events.push(event);
            }
            let mut replies = vec![
                authenticated_resp(),
                count_resp(16),
                (OP_FETCH_RESP, fetch_batch(&events)),
            ];
            for _ in &events {
                replies.push(binding_resp(&a));
            }
            replies.push((
                OP_DELIVERY_ACK,
                (events.len() as u64).to_be_bytes().to_vec(),
            ));
            let result = b.fetch_and_decrypt(&mut Fake::new(replies)).await.unwrap();
            if delete {
                assert!(result.received.is_empty());
            } else {
                assert_eq!(result.received.len(), 1);
                assert_eq!(result.received[0].text, "final effective text");
            }
            assert_eq!(crate::store::inbox_count(&b.conn).unwrap(), 1);
            drop(a);
            drop(b);
            std::fs::remove_dir_all(da).unwrap();
            std::fs::remove_dir_all(db).unwrap();
        }
    }

    #[tokio::test]
    async fn crypto_loop_through_doubles() {
        crypto_loop(false).await;
    }

    #[tokio::test]
    async fn encrypted_ratchet_outbox_retry_and_inbox_survive_reopen() {
        crypto_loop(true).await;
    }

    async fn crypto_loop(encrypted: bool) {
        let suffix = if encrypted { "sealed" } else { "plain" };
        let (mut a, da) = tmp_core_mode(
            &format!("loop-a-{suffix}"),
            if encrypted { Some(&[9; 32]) } else { None },
        );
        let (mut b, db) = tmp_core_mode(
            &format!("loop-b-{suffix}"),
            if encrypted { Some(&[8; 32]) } else { None },
        );
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        // Настоящий one-time Боба для CLAIM-ответа (подписан его identity).
        b.account.generate_one_time_keys(1);
        olm::persist(&b.conn, &b.account, b.next_key_id).unwrap();
        let ot = *b
            .account
            .one_time_keys()
            .values()
            .next()
            .expect("ot")
            .as_bytes();
        let bdev = b.device_pub();
        let bsig = olm::sign_prekey(&b.account, &bdev, 5, &ot);
        let mut prekey = 5u32.to_be_bytes().to_vec();
        prekey.extend_from_slice(&ot);
        let _ = bsig; // подпись уже проверена сервером при upload; в CLAIM-ответе её нет
        let mut fa = Fake::new(vec![
            authenticated_resp(),
            binding_resp(&b),
            count_resp(16),
            (OP_PREKEY, prekey),
        ]);
        fa.echo_send_ack = true;
        let mid = a
            .send_text(&mut fa, "BOBB00000002", "hello bob")
            .await
            .expect("send");
        // Outbox: ciphertext сохранён, статус accepted.
        assert_eq!(
            status_of(&a.conn, &mid).expect("st"),
            crate::store::outbox_status::ACCEPTED
        );
        let outgoing_history = a.history_page("BOBB00000002", None, 100).unwrap();
        assert_eq!(outgoing_history.rows.len(), 1);
        assert_eq!(outgoing_history.rows[0].text, "hello bob");
        assert_eq!(
            outgoing_history.rows[0].delivery_state,
            Some(crate::history::DeliveryState::Accepted)
        );
        // Ретрай после accept: шлёт ТОТ ЖЕ ciphertext (сравниваем байты).
        let sent_ct = fa
            .sent
            .iter()
            .find(|(op, _)| *op == OP_SEND)
            .expect("send")
            .1[32..]
            .to_vec();
        if encrypted {
            drop(a);
            a = Core::open_encrypted(&da.join("core.db"), &[9; 32]).expect("reopen a ratchet");
            assert_eq!(a.outbox_ciphertext(&mid).unwrap(), sent_ct);
            assert!(crate::store::load_session(&a.conn, "BOBB00000002")
                .unwrap()
                .is_some());
        }
        let mut fa2 = Fake::with_echo();
        fa2.replies.push_back(authenticated_resp());
        fa2.replies.push_back(binding_resp(&b));
        let stats = a.retry_queued(&mut fa2).await.expect("retry");
        assert_eq!((stats.resent, stats.skipped), (1, 0));
        let resent_ct = fa2
            .sent
            .iter()
            .find(|(op, _)| *op == OP_SEND)
            .expect("resend")
            .1[32..]
            .to_vec();
        assert_eq!(
            sent_ct, resent_ct,
            "retry must reuse saved ciphertext, not re-encrypt"
        );
        assert_eq!(
            a.history_page("BOBB00000002", None, 100).unwrap(),
            outgoing_history
        );
        // Приём Бобом: FETCH_RESP с серверным событием + ack cursor.
        let a_dev = a.device_pub();
        let mut fe = vec![0u8, 1];
        fe.extend_from_slice(&1u64.to_be_bytes());
        fe.extend_from_slice(&a_dev);
        fe.extend_from_slice(&a.my_account().unwrap().0);
        fe.extend_from_slice(&mid);
        fe.extend_from_slice(&(sent_ct.len() as u16).to_be_bytes());
        fe.extend_from_slice(&sent_ct);
        let mut fb = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, fe),
            binding_resp(&a),
            (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
        ]);
        let res = b.fetch_and_decrypt(&mut fb).await.expect("fetch");
        assert_eq!(res.received.len(), 1);
        assert_eq!(res.received[0].text, "hello bob");
        assert_eq!(res.received[0].contact_id, "ALICE0000001");
        assert_eq!(res.cursor, 1);
        // A replay after reopening must use durable inbox dedup, before loading
        // or advancing the persisted Olm ratchet (plain and encrypted stores).
        drop(b);
        b = if encrypted {
            Core::open_encrypted(&db.join("core.db"), &[8; 32]).expect("reopen b")
        } else {
            Core::open(&db.join("core.db")).expect("reopen b")
        };
        let session_before = crate::store::load_session(&b.conn, "ALICE0000001").unwrap();
        let account_before = serde_json::to_string(&b.account.pickle()).unwrap();
        assert!(session_before.is_some());
        // Повтор той же пачки (reorder/replay): локальный дедуп — тишина.
        let mut fe2 = vec![0u8, 1];
        fe2.extend_from_slice(&1u64.to_be_bytes());
        fe2.extend_from_slice(&a_dev);
        fe2.extend_from_slice(&a.my_account().unwrap().0);
        fe2.extend_from_slice(&mid);
        fe2.extend_from_slice(&(sent_ct.len() as u16).to_be_bytes());
        fe2.extend_from_slice(&sent_ct);
        let mut fb2 = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, fe2),
            (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
        ]);
        let res2 = b.fetch_and_decrypt(&mut fb2).await.expect("fetch2");
        assert_eq!(
            res2,
            FetchResult {
                cursor: 1,
                ..Default::default()
            },
            "durable replay must not be classified as a decryption failure"
        );
        assert!(
            fb2.sent
                .iter()
                .all(|(op, _)| *op != dmsg_protocol::OP_DEVICE_BINDING),
            "durable replay must be deduplicated before refreshing peer trust"
        );
        assert_eq!(
            crate::store::load_session(&b.conn, "ALICE0000001").unwrap(),
            session_before
        );
        assert_eq!(
            serde_json::to_string(&b.account.pickle()).unwrap(),
            account_before
        );
        assert_eq!(crate::store::inbox_count(&b.conn).expect("count"), 1);
        let incoming = b.history_page("ALICE0000001", None, 100).unwrap();
        assert_eq!(incoming.rows.len(), 1);
        assert_eq!(incoming.rows[0].text, "hello bob");
        assert_eq!(
            incoming.rows[0].direction,
            crate::history::MessageDirection::Incoming
        );
        assert_eq!(incoming.rows[0].delivery_state, None);
        assert_eq!(b.dialogs_page(None, 100).unwrap().rows[0].local_unread, 1);
        // The dedup key includes the sender; new/tampered events still fail.
        let mut event = RawEvent {
            seq: 2,
            sender: a_dev,
            sender_user: a.my_account().unwrap().0,
            message_id: [42; 16],
            ciphertext: sent_ct,
        };
        assert!(matches!(
            b.decrypt_event(&event).await,
            Err(Fail::Skip(EventSkip::Undecryptable))
        ));
        event.message_id = mid;
        event.sender = [42; 32];
        event.sender_user = [42; 16];
        assert!(matches!(
            b.decrypt_event(&event).await,
            Err(Fail::Skip(EventSkip::Unknown))
        ));
        event.sender = a_dev;
        event.sender_user = a.my_account().unwrap().0;
        event.message_id = [43; 16];
        let ciphertext = std::mem::replace(&mut event.ciphertext, vec![9, 1, 2, 3]);
        assert!(matches!(
            b.decrypt_event(&event).await,
            Err(Fail::Skip(EventSkip::Undecryptable))
        ));
        if encrypted {
            drop(b);
            b = Core::open_encrypted(&db.join("core.db"), &[8; 32]).expect("reopen b inbox");
            assert_eq!(
                crate::store::inbox_list(&b.conn, 0, 10).unwrap().0[0].2,
                "hello bob"
            );
            let raw = std::fs::read(db.join("core.db")).unwrap();
            assert!(!raw.windows(b"hello bob".len()).any(|w| w == b"hello bob"));
        }

        // A failed durable lookup must abort the batch without DELIVERY_ACK.
        b.conn.execute("DROP TABLE core_messages", []).unwrap();
        let mut fe3 = vec![0, 1];
        fe3.extend_from_slice(&1u64.to_be_bytes());
        fe3.extend_from_slice(&a_dev);
        fe3.extend_from_slice(&a.my_account().unwrap().0);
        fe3.extend_from_slice(&mid);
        fe3.extend_from_slice(&(ciphertext.len() as u16).to_be_bytes());
        fe3.extend_from_slice(&ciphertext);
        let mut fb3 = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, fe3),
            (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
        ]);
        assert!(matches!(
            b.fetch_and_decrypt(&mut fb3).await,
            Err(OlmError::Store(_))
        ));
        assert!(!fb3.sent.iter().any(|(op, _)| *op == OP_DELIVERY_ACK));
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn empty_prekeys_is_explicit_error() {
        let (mut a, da) = tmp_core("empty-a");
        let (mut b, db) = tmp_core("empty-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        // COUNT высокий (свой запас есть), CLAIM — пустой запас пира.
        let mut fa = Fake::new(vec![
            authenticated_resp(),
            binding_resp(&b),
            count_resp(16),
            (OP_ERROR, vec![dmsg_protocol::ERR_NO_PREKEY]),
        ]);
        let r = a.send_text(&mut fa, "BOBB00000002", "hi").await;
        assert_eq!(r, Err(OlmError::NoPeerPrekeys));
        // Ничего не сохранено (шифровать было не для кого — нет сессии).
        assert!(crate::store::outbox_queued(&a.conn, 0, 32)
            .expect("q")
            .0
            .is_empty());
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn concurrent_offline_queue_advances_persisted_ratchet_once_per_message() {
        let (mut a, da) = tmp_core("offline-concurrent-a");
        let (mut b, db) = tmp_core("offline-concurrent-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        b.account.generate_one_time_keys(1);
        olm::persist(&b.conn, &b.account, b.next_key_id).unwrap();
        let ot = *b
            .account
            .one_time_keys()
            .values()
            .next()
            .expect("ot")
            .as_bytes();
        let (peer_ed, peer_curve) = b.identity_keys();
        let session = olm::outbound(&a.account, &peer_curve, &ot).expect("session");
        crate::store::save_session(
            &a.conn,
            "BOBB00000002",
            &olm::pickle_session(&session).expect("pickle"),
            &peer_ed,
            &peer_curve,
        )
        .expect("session store");
        drop(a);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for text in ["first offline", "second offline"] {
            let path = da.join("core.db");
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut core = Core::open(&path).expect("reopen");
                barrier.wait();
                core.queue_text_existing_session("BOBB00000002", text)
                    .expect("queued")
                    .expect("session")
            }));
        }
        let mids: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();
        assert_ne!(mids[0], mids[1]);
        let a = Core::open(&da.join("core.db")).expect("reopen after queue");
        let rows = crate::store::outbox_queued(&a.conn, 0, 32)
            .expect("outbox")
            .0;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            a.history_page("BOBB00000002", None, 100)
                .unwrap()
                .rows
                .len(),
            2
        );
        assert!(rows
            .iter()
            .all(|r| r.4 == crate::store::outbox_status::QUEUED));
        let mut batch = vec![0, 2];
        for (seq, (_, mid, _, ct, _)) in rows.iter().enumerate() {
            assert_eq!(a.outbox_ciphertext(mid).expect("ciphertext"), *ct);
            batch.extend_from_slice(&((seq + 1) as u64).to_be_bytes());
            batch.extend_from_slice(&a.device_pub());
            batch.extend_from_slice(&a.my_account().unwrap().0);
            batch.extend_from_slice(mid);
            batch.extend_from_slice(&(ct.len() as u16).to_be_bytes());
            batch.extend_from_slice(ct);
        }
        let mut fake = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, batch),
            binding_resp(&a),
            binding_resp(&a),
            (OP_DELIVERY_ACK, 2u64.to_be_bytes().to_vec()),
        ]);
        let result = b
            .fetch_and_decrypt(&mut fake)
            .await
            .expect("decrypt in ratchet order");
        assert_eq!(result.received.len(), 2);
        assert_eq!(
            b.history_page("ALICE0000001", None, 100)
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(result.skipped_undecryptable, 0);
        let texts: Vec<_> = result.received.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.contains(&"first offline") && texts.contains(&"second offline"));
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn substitution_stops_send_before_network() {
        let (mut a, da) = tmp_core("stop-a");
        let (mut b, db) = tmp_core("stop-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        // Подмена: Боб «перевыпустил» QR с тем же ID.
        let (buid, _) = b.my_account().expect("b account");
        let evil = contacts::build_qr(
            "BOBB00000002",
            &buid,
            &b.device_pub(),
            &[9u8; 32],
            &[8u8; 32],
        )
        .expect("evil");
        assert_eq!(
            contacts::add_from_qr(&a.conn, &evil),
            Ok(contacts::QrResult::IdentityChanged)
        );
        // Transport, падающий при любом использовании: отказ обязан быть до сети.
        struct Dead;
        impl Transport for Dead {
            async fn connect(&mut self) -> Result<(), TransportError> {
                panic!("no network expected")
            }
            async fn send_frame(&mut self, _o: u8, _p: &[u8]) -> Result<(), TransportError> {
                panic!("no network expected")
            }
            async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
                panic!("no network expected")
            }
            async fn close(&mut self) {}
            fn is_connected(&self) -> bool {
                false
            }
        }
        let mut dead = Dead;
        assert_eq!(
            a.send_text(&mut dead, "BOBB00000002", "hi").await,
            Err(OlmError::IdentityMismatch)
        );
        // Explicit confirm permits the RESUME + binding + COUNT path.
        contacts::confirm_identity(&a.conn, "BOBB00000002").expect("confirm");
        let c = contacts::get(&a.conn, "BOBB00000002").unwrap().unwrap();
        let binding = dmsg_protocol::auth::build_binding(
            &c.user_id.unwrap(),
            &c.device_key.unwrap(),
            &c.ed_identity.unwrap(),
            &c.curve_identity.unwrap(),
        );
        let mut fa = Fake::new(vec![
            authenticated_resp(),
            (dmsg_protocol::OP_DEVICE_BINDING_RESP, binding),
            count_resp(16),
        ]);
        // CLAIM вернёт реальный ключ НОВОГО боба? У тестового боба старые ключи —
        // подпись не сойдётся с новым пином... точнее: серверный путь здесь
        // пропущен (double), claim-ответ соберём от имени нового identity:
        // проще assert, что дело дошло до сети (COUNT съеден, дальше CLAIM).
        let r = a.send_text(&mut fa, "BOBB00000002", "hi").await;
        assert!(
            matches!(r, Err(OlmError::Transport(_))),
            "must reach network after confirm, got {r:?}"
        );
        assert_eq!(fa.sent.iter().filter(|(op, _)| *op == OP_COUNT).count(), 1);
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn unknown_wire_and_version_are_explicit() {
        let (mut b, db) = tmp_core("wire-b");
        let (mut a, da) = tmp_core("wire-a");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        // Ciphertext с неизвестным Olm-type (9): пропуск + ack, без паники.
        let a_dev = a.device_pub();
        let mid = [7u8; 16];
        let mut fe = vec![0u8, 1];
        fe.extend_from_slice(&3u64.to_be_bytes());
        fe.extend_from_slice(&a_dev);
        fe.extend_from_slice(&a.my_account().unwrap().0);
        fe.extend_from_slice(&mid);
        let bad_ct = vec![9u8, 1, 2, 3];
        fe.extend_from_slice(&(bad_ct.len() as u16).to_be_bytes());
        fe.extend_from_slice(&bad_ct);
        let mut fb = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, fe),
            binding_resp(&a),
            (OP_DELIVERY_ACK, 0u64.to_be_bytes().to_vec()),
        ]);
        let res = b.fetch_and_decrypt(&mut fb).await.expect("fetch");
        assert!(res.received.is_empty());
        assert_eq!(res.skipped_undecryptable, 1);
        assert_eq!(res.cursor, 0);
        assert_eq!(
            fb.sent
                .iter()
                .find(|(op, _)| *op == OP_DELIVERY_ACK)
                .unwrap()
                .1,
            vec![0, 0],
            "invalid ciphertext must be retained"
        );
        // Кадр с неизвестной wire-версией — явная ошибка.
        let mut f = dmsg_protocol::encode_frame(OP_FETCH_RESP, b"x").expect("frame");
        f[0] = 9;
        assert_eq!(decode_app_frame(&f), Err(OlmError::WireVersion(9)));
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn noise_only_replacement_warns_and_stops_before_claim_or_send() {
        let (mut a, da) = tmp_core("noise-change-a");
        let (mut b, db) = tmp_core("noise-change-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        let (ed, curve) = b.identity_keys();
        crate::store::save_session(&a.conn, "BOBB00000002", "old session sentinel", &ed, &curve)
            .unwrap();
        let changed =
            dmsg_protocol::auth::build_binding(&b.my_account().unwrap().0, &[42; 32], &ed, &curve);
        let mut t = Fake::new(vec![
            authenticated_resp(),
            (dmsg_protocol::OP_DEVICE_BINDING_RESP, changed),
        ]);
        assert_eq!(
            a.send_text(&mut t, "BOBB00000002", "must stop").await,
            Err(OlmError::IdentityMismatch)
        );
        assert_eq!(
            t.sent.iter().map(|(op, _)| *op).collect::<Vec<_>>(),
            vec![OP_RESUME, dmsg_protocol::OP_DEVICE_BINDING]
        );
        let c = contacts::get(&a.conn, "BOBB00000002").unwrap().unwrap();
        assert_eq!(c.device_key, Some(b.device_pub()));
        assert_eq!(c.seen_device, Some([42; 32]));
        assert_eq!(c.ed_identity, Some(ed));
        assert_eq!(c.curve_identity, Some(curve));
        a.confirm_contact("BOBB00000002").unwrap();
        let c = contacts::get(&a.conn, "BOBB00000002").unwrap().unwrap();
        assert_eq!(c.device_key, Some([42; 32]));
        assert!(c.seen_device.is_none());
        assert!(crate::store::load_session(&a.conn, "BOBB00000002")
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db).ok();
    }

    #[tokio::test]
    async fn failed_integrity_retains_event_without_advancing_account_or_session() {
        let (mut a, da) = tmp_core("integrity-a");
        let (mut b, db) = tmp_core("integrity-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        b.account.generate_one_time_keys(1);
        olm::persist(&b.conn, &b.account, b.next_key_id).unwrap();
        let ot = *b
            .account
            .one_time_keys()
            .values()
            .next()
            .unwrap()
            .as_bytes();
        let mut outbound =
            olm::outbound(&a.account, &olm::curve_identity(&b.account), &ot).unwrap();
        let mut event = dmsg_protocol::e2e::Event {
            message_id: [1; 16],
            sender_ed: olm::ed_identity(&a.account),
            body: dmsg_protocol::e2e::Body::Text("integrity message".into()),
        };
        let plaintext = dmsg_protocol::e2e::encode(&event).unwrap();
        let first = RawEvent {
            seq: 1,
            sender: a.device_pub(),
            sender_user: a.my_account().unwrap().0,
            message_id: [1; 16],
            ciphertext: olm::encode_wire(&outbound.encrypt(&plaintext).unwrap()),
        };
        assert!(b.decrypt_event(&first).await.unwrap().is_some());
        let reverse = b.persist_text("ALICE0000001", "response").unwrap().unwrap();
        let _ = outbound
            .decrypt(&olm::decode_wire(&reverse.2).unwrap())
            .unwrap();
        event.message_id = [2; 16];
        let valid = olm::encode_wire(
            &outbound
                .encrypt(dmsg_protocol::e2e::encode(&event).unwrap())
                .unwrap(),
        );
        assert_eq!(valid[0], 1);
        let mut broken = valid.clone();
        *broken.last_mut().unwrap() ^= 1;
        let session = crate::store::load_session(&b.conn, "ALICE0000001").unwrap();
        let account = crate::store::load_olm(&b.conn).unwrap();
        let batch = |ct: &[u8]| {
            let mut p = vec![0, 1];
            p.extend_from_slice(&2u64.to_be_bytes());
            p.extend_from_slice(&a.device_pub());
            p.extend_from_slice(&a.my_account().unwrap().0);
            p.extend_from_slice(&[2; 16]);
            p.extend_from_slice(&(ct.len() as u16).to_be_bytes());
            p.extend_from_slice(ct);
            p
        };
        let mut t = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, batch(&broken)),
            binding_resp(&a),
            (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
        ]);
        let result = b.fetch_and_decrypt(&mut t).await.unwrap();
        assert_eq!(result.skipped_undecryptable, 1);
        assert!(result.received.is_empty());
        assert_eq!(
            t.sent
                .iter()
                .find(|(op, _)| *op == OP_DELIVERY_ACK)
                .unwrap()
                .1,
            vec![0, 0]
        );
        assert_eq!(
            session,
            crate::store::load_session(&b.conn, "ALICE0000001").unwrap()
        );
        assert_eq!(account, crate::store::load_olm(&b.conn).unwrap());
        let mut t = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, batch(&valid)),
            binding_resp(&a),
            (OP_DELIVERY_ACK, 2u64.to_be_bytes().to_vec()),
        ]);
        assert_eq!(b.fetch_and_decrypt(&mut t).await.unwrap().received.len(), 1);
        let mut t = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, batch(&valid)),
            (OP_DELIVERY_ACK, 2u64.to_be_bytes().to_vec()),
        ]);
        assert!(b
            .fetch_and_decrypt(&mut t)
            .await
            .unwrap()
            .received
            .is_empty());
        assert_eq!(crate::store::inbox_count(&b.conn).unwrap(), 2);
        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db).ok();
    }

    #[tokio::test]
    async fn history_failure_rolls_back_ratchets_outbox_inbox_and_activity_without_ack() {
        let (mut a, da) = tmp_core_mode("history-rollback-a", Some(&[17; 32]));
        let (mut b, db) = tmp_core_mode("history-rollback-b", Some(&[18; 32]));
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        b.account.generate_one_time_keys(1);
        olm::persist(&b.conn, &b.account, b.next_key_id).unwrap();
        let ot = *b
            .account
            .one_time_keys()
            .values()
            .next()
            .unwrap()
            .as_bytes();
        let (ed, curve) = b.identity_keys();
        let session = olm::outbound(&a.account, &curve, &ot).unwrap();
        crate::store::save_session(
            &a.conn,
            "BOBB00000002",
            &olm::pickle_session(&session).unwrap(),
            &ed,
            &curve,
        )
        .unwrap();
        let session_before = crate::store::load_session(&a.conn, "BOBB00000002").unwrap();
        let dialog_before = a.dialogs_page(None, 100).unwrap();
        // Fail AFTER ratchet, outbox and history writes, at the summary write.
        a.conn.execute_batch("CREATE TRIGGER reject_activity BEFORE UPDATE OF local_activity_ms ON core_contacts BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        assert!(matches!(
            a.queue_text_existing_session("BOBB00000002", "rollback text"),
            Err(OlmError::Store(_))
        ));
        assert_eq!(
            crate::store::load_session(&a.conn, "BOBB00000002").unwrap(),
            session_before
        );
        assert!(crate::store::outbox_queued(&a.conn, 0, 100)
            .unwrap()
            .0
            .is_empty());
        assert!(a
            .history_page("BOBB00000002", None, 100)
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(a.dialogs_page(None, 100).unwrap(), dialog_before);
        drop(a);
        a = Core::open_encrypted(&da.join("core.db"), &[17; 32]).unwrap();
        assert_eq!(
            crate::store::load_session(&a.conn, "BOBB00000002").unwrap(),
            session_before
        );
        a.conn
            .execute_batch("DROP TRIGGER reject_activity")
            .unwrap();
        let mid = a
            .queue_text_existing_session("BOBB00000002", "rollback text")
            .unwrap()
            .unwrap();
        let wire = a.outbox_ciphertext(&mid).unwrap();
        let mut batch = vec![0, 1];
        batch.extend_from_slice(&1u64.to_be_bytes());
        batch.extend_from_slice(&a.device_pub());
        batch.extend_from_slice(&a.my_account().unwrap().0);
        batch.extend_from_slice(&mid);
        batch.extend_from_slice(&(wire.len() as u16).to_be_bytes());
        batch.extend_from_slice(&wire);
        let account_before = crate::store::load_olm(&b.conn).unwrap();
        let dialog_before = b.dialogs_page(None, 100).unwrap();
        b.conn.execute_batch("CREATE TRIGGER reject_history AFTER INSERT ON core_messages BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        let fake = || {
            Fake::new(vec![
                authenticated_resp(),
                count_resp(16),
                (OP_FETCH_RESP, batch.clone()),
                binding_resp(&a),
                (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
            ])
        };
        let mut t = fake();
        assert!(matches!(
            b.fetch_and_decrypt(&mut t).await,
            Err(OlmError::Store(_))
        ));
        assert!(!t.sent.iter().any(|(op, _)| *op == OP_DELIVERY_ACK));
        assert_eq!(crate::store::load_olm(&b.conn).unwrap(), account_before);
        assert!(crate::store::load_session(&b.conn, "ALICE0000001")
            .unwrap()
            .is_none());
        assert_eq!(crate::store::inbox_count(&b.conn).unwrap(), 0);
        assert!(b
            .history_page("ALICE0000001", None, 100)
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(b.dialogs_page(None, 100).unwrap(), dialog_before);
        drop(b);
        b = Core::open_encrypted(&db.join("core.db"), &[18; 32]).unwrap();
        assert_eq!(crate::store::load_olm(&b.conn).unwrap(), account_before);
        b.conn.execute_batch("DROP TRIGGER reject_history").unwrap();
        assert_eq!(
            b.fetch_and_decrypt(&mut fake())
                .await
                .unwrap()
                .received
                .len(),
            1
        );
        assert_eq!(
            b.history_page("ALICE0000001", None, 100)
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(b.dialogs_page(None, 100).unwrap().rows[0].local_unread, 1);
        let saved_session = crate::store::load_session(&b.conn, "ALICE0000001").unwrap();
        let mut replay = Fake::new(vec![
            authenticated_resp(),
            count_resp(16),
            (OP_FETCH_RESP, batch),
            (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec()),
        ]);
        assert!(b
            .fetch_and_decrypt(&mut replay)
            .await
            .unwrap()
            .received
            .is_empty());
        assert_eq!(
            b.history_page("ALICE0000001", None, 100)
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(
            crate::store::load_session(&b.conn, "ALICE0000001").unwrap(),
            saved_session
        );
        drop(a);
        drop(b);
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }
}
