//! K3 сессии 1-на-1 + outbox + приём.
//!
//! Инварианты (ARCH §7, план K3):
//! - ratchet и ciphertext-outbox — ОДНА TX: сессия читается после write-lock,
//!   шифруется локально, затем `core_sessions`-пикл + `core_outbox`-строка
//!   коммитятся вместе; при ошибке TX локальная копия отбрасывается
//!   (перешифровка — только несохранённого/нового; ретрай шлёт ТОЛЬКО
//!   сохранённый ciphertext с тем же message_id — сервер дедуплицирует);
//! - статусы outbox: queued → accepted (SEND_ACK ST_ACCEPTED) → delivered
//!   (SEND_ACK ST_DELIVERED при повторе после доставки);
//! - plaintext-конверт внутри Olm: `[sender_ed_identity 32][text UTF-8]`;
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
//! Сессия сервера — per-connection: после каждого WELCOME нужен ENROL
//! (иначе mailbox закрывается). [`Core::login`] шлёт ENROL-replay
//! сохранённым token (идемпотентен в пределах TTL invite) и сверяет
//! ENROLLED с хранимым account; каждый сетевой метод логинится сам.

use dmsg_protocol::{
    decode_frame, mailbox as mp, OP_DELIVERY_ACK, OP_ENROL, OP_ENROLLED, OP_FETCH,
    OP_FETCH_RESP, OP_SEND, OP_SEND_ACK, OP_ERROR, ST_ACCEPTED,
    ST_DELIVERED, TEXT_MAX,
};

use crate::contacts::{self, Contact};
use crate::olm::{self, OlmError};
use crate::transport::Transport;

/// Сырое событие mailbox (без decrypt — для evidence и диагностики K4).
pub struct RawEvent {
    pub seq: u64,
    pub sender: [u8; 32],
    pub message_id: [u8; 16],
    pub ciphertext: Vec<u8>,
}

/// Расшифрованное входящее.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub contact_id: String,
    pub text: String,
    pub message_id: [u8; 16],
    pub seq: u64,
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

/// Ядро K3: соединение с БД + Olm Account + кэш сессий + ключи устройства.
/// Одно на процесс (как Supervisor); `Connection` не Sync — не шарить.
pub struct Core {
    conn: rusqlite::Connection,
    account: vodozemac::olm::Account,
    next_key_id: u32,
    device_pub: [u8; 32],
    sessions: std::collections::HashMap<String, vodozemac::olm::Session>,
}

impl Core {
    /// Открыть ядро: store + Account (создаётся при первом запуске) +
    /// ключи устройства из enrol. Без enrol — NotEnrolled (явно).
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
        let device_priv =
            crate::store::load_identity(&conn).map_err(OlmError::Store)?.ok_or(OlmError::NotEnrolled)?;
        let (account, next_key_id) = olm::load_or_create(&conn)?;
        let device_pub = olm::device_pubkey(&device_priv);
        Ok(Self { conn, account, next_key_id, device_pub, sessions: Default::default() })
    }

    /// Свои Olm identity-ключи (ed, curve) — для contact-QR.
    pub fn identity_keys(&self) -> ([u8; 32], [u8; 32]) {
        (olm::ed_identity(&self.account), olm::curve_identity(&self.account))
    }

    /// Свой user_id/contact_id из ENROLLED (для QR и диагностики).
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

    /// Явное подтверждение подмены: pin = seen, кэш сессии сброшен.
    pub fn confirm_contact(&mut self, contact_id: &str) -> Result<(), OlmError> {
        contacts::confirm_identity(&self.conn, contact_id)?;
        self.sessions.remove(contact_id);
        Ok(())
    }

    /// Reconnect-побудка refill: login (ENROL-replay) + COUNT + догрузка.
    /// Зовёт шелл после переподключения (планировщика нет).
    pub async fn on_reconnect(&mut self, t: &mut impl Transport) -> Result<u32, OlmError> {
        self.login(t).await?;
        olm::ensure_prekeys(&self.conn, &mut self.account, t, &self.device_pub, &mut self.next_key_id)
            .await
    }

    /// Login: ENROL-replay сохранённым token на этом коннекте.
    /// Сервер держит enrolled-флаг per-connection: без ENROL после WELCOME
    /// доступны только повторный ENROL, mailbox закрывается. Replay тем же
    /// ключом идемпотентен (K2); ENROLLED сверяется с хранимым account
    /// (fail-closed при расхождении). Каждый сетевой метод зовёт login
    /// сам — переподключение супервизора прозрачно.
    pub async fn login(&mut self, t: &mut impl Transport) -> Result<(), OlmError> {
        let token =
            crate::store::load_token(&self.conn).map_err(OlmError::Store)?.ok_or(OlmError::NotEnrolled)?;
        t.send_frame(OP_ENROL, &token)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) =
            t.recv_frame().await.map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_ENROLLED {
            if p.len() != 28 {
                return Err(OlmError::Protocol("bad enrolled len"));
            }
            let mut user_id = [0u8; 16];
            user_id.copy_from_slice(&p[..16]);
            let contact_id =
                std::str::from_utf8(&p[16..]).map_err(|_| OlmError::Protocol("bad contact"))?;
            let (my_uid, my_cid) = self.my_account()?;
            if user_id != my_uid || contact_id != my_cid {
                return Err(OlmError::Protocol("enrolled mismatch"));
            }
            return Ok(());
        }
        if op == OP_ERROR && p.len() == 1 {
            return Err(map_enrol_error(p[0]));
        }
        Err(OlmError::Protocol("unexpected enrol reply"))
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
        olm::ensure_prekeys(&self.conn, &mut self.account, t, &self.device_pub, &mut self.next_key_id)
            .await?;
        self.session_for(&c, &peer_ed, &peer_curve, t, &device_key).await?;
        let (message_id, user_id, wire) = self.persist_text(contact_id, text)?
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
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
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
        let mut session = olm::unpickle_session(&pickle)?;
        let mut plain = Vec::with_capacity(32 + text.len());
        plain.extend_from_slice(&olm::ed_identity(&self.account));
        plain.extend_from_slice(text.as_bytes());
        let msg = session.encrypt(&plain).map_err(|_| OlmError::Crypto("encrypt"))?;
        let wire = olm::encode_wire(&msg);
        if wire.len() > dmsg_protocol::CIPHERTEXT_MAX {
            return Err(OlmError::Protocol("ciphertext too long"));
        }
        let mut message_id = [0u8; 16];
        getrandom::fill(&mut message_id).map_err(|_| OlmError::Crypto("rng"))?;
        let spickle = olm::pickle_session(&session)?;
        tx.execute(
            "UPDATE core_sessions SET pickle=dmsg_seal('session_pickle',?2)
             WHERE contact_id=?1",
            rusqlite::params![contact_id, spickle],
        )
        .map_err(|e| OlmError::Store(format!("session: {e}")))?;
        tx.execute(
            "INSERT INTO core_outbox(message_id, contact_id, ciphertext, status)
             VALUES(?1,?2,?3,'queued')",
            rusqlite::params![message_id.as_slice(), contact_id, wire.as_slice()],
        )
        .map_err(|e| OlmError::Store(format!("outbox: {e}")))?;
        tx.commit().map_err(|e| OlmError::Store(format!("commit: {e}")))?;
        self.sessions.remove(contact_id);
        Ok(Some((message_id, user_id, wire)))
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
                    Err(_) => {
                        stats.skipped += 1;
                    }
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
                "SELECT ciphertext FROM core_outbox WHERE message_id=?1",
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
        let (op, p) = t.recv_frame().await.map_err(|e| OlmError::Transport(e.to_string()))?;
        if op == OP_FETCH_RESP {
            let events = mp::parse_fetch_resp(&p).ok_or(OlmError::Protocol("bad fetch"))?;
            return events
                .into_iter()
                .map(|e| {
                    Ok(RawEvent {
                        seq: e.seq,
                        sender: e.sender.try_into().map_err(|_| OlmError::Protocol("bad sender"))?,
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
        let (op, p) = t.recv_frame().await.map_err(|e| OlmError::Transport(e.to_string()))?;
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
        olm::ensure_prekeys(&self.conn, &mut self.account, t, &self.device_pub, &mut self.next_key_id)
            .await?;
        let events = self.fetch_raw(t).await?;
        let mut res = FetchResult::default();
        let mut seqs: Vec<u64> = Vec::with_capacity(events.len());
        for e in &events {
            seqs.push(e.seq);
            match self.decrypt_event(e).await {
                Ok(Some(r)) => res.received.push(r),
                Ok(None) => {}
                Err(Fail::Skip(EventSkip::Unknown)) => res.skipped_unknown += 1,
                Err(Fail::Skip(EventSkip::Blocked)) => res.skipped_blocked += 1,
                Err(Fail::Skip(EventSkip::Undecryptable)) => res.skipped_undecryptable += 1,
                Err(Fail::Skip(EventSkip::Mismatch)) => res.skipped_mismatch += 1,
                Err(Fail::Err(e)) => return Err(e),
            }
        }
        res.cursor = self.ack(t, &seqs).await?;
        Ok(res)
    }

    /// Расшифровать одно событие. Ok(None) — дедуп-повтор (уже лежит).
    /// Skip — пропуск со счётчиком (cursor всё равно двинется через ack).
    /// Fail::Err — жёсткая ошибка (store), прерывает пачку.
    async fn decrypt_event(&mut self, e: &RawEvent) -> Result<Option<Received>, Fail> {
        // At-least-once replay cannot be decrypted twice by an Olm ratchet.
        // Only a durable inbox row permits this early return; a lookup failure
        // aborts the batch before ACK, and unseen events still undergo checks.
        let received: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM core_inbox WHERE sender_device=?1 AND message_id=?2)",
                rusqlite::params![e.sender.as_slice(), e.message_id.as_slice()],
                |r| r.get(0),
            )
            .map_err(|e| OlmError::Store(format!("inbox dedup: {e}")))?;
        if received {
            return Ok(None);
        }
        let c = match contacts::get_by_device(&self.conn, &e.sender)? {
            Some(c) => c,
            None => return Err(Fail::Skip(EventSkip::Unknown)),
        };
        if c.state == contacts::state::BLOCKED {
            return Err(Fail::Skip(EventSkip::Blocked));
        }
        let peer_ed = c.ed_identity.ok_or(Fail::Skip(EventSkip::Undecryptable))?;
        let peer_curve = c.curve_identity.ok_or(Fail::Skip(EventSkip::Undecryptable))?;
        let msg = olm::decode_wire(&e.ciphertext).map_err(|_| Fail::Skip(EventSkip::Undecryptable))?;
        // Сессия из кэша/БД; prekey без сессии — создать inbound.
        if !self.sessions.contains_key(&c.contact_id) {
            if let Some((pickle, row_ed, row_curve)) =
                crate::store::load_session(&self.conn, &c.contact_id).map_err(OlmError::Store)?
            {
                if row_ed != peer_ed || row_curve != peer_curve {
                    return Err(Fail::Skip(EventSkip::Mismatch));
                }
                let s = olm::unpickle_session(&pickle)?;
                self.sessions.insert(c.contact_id.clone(), s);
            }
        }
        let plaintext: Vec<u8> = match msg {
            vodozemac::olm::OlmMessage::PreKey(ref pre) => {
                if self.sessions.contains_key(&c.contact_id) {
                    let s = self.sessions.get_mut(&c.contact_id).expect("checked");
                    s.decrypt(&msg).map_err(|_| Fail::Skip(EventSkip::Undecryptable))?
                } else {
                    let presented = *pre.identity_key().as_bytes();
                    match olm::inbound(&mut self.account, &peer_curve, pre) {
                        Ok((s, _, pt)) => {
                            self.sessions.insert(c.contact_id.clone(), s);
                            pt
                        }
                        Err(OlmError::IdentityMismatch) => {
                            // Подмена с receive-path: фиксируем presented
                            // (только если реально отличается от пина).
                            if presented != peer_curve {
                                contacts::note_presented_curve(
                                    &self.conn,
                                    &c.contact_id,
                                    &presented,
                                )?;
                            }
                            return Err(Fail::Skip(EventSkip::Mismatch));
                        }
                        Err(e) => return Err(Fail::Err(e)),
                    }
                }
            }
            vodozemac::olm::OlmMessage::Normal(_) => {
                if !self.sessions.contains_key(&c.contact_id) {
                    return Err(Fail::Skip(EventSkip::Undecryptable));
                }
                let s = self.sessions.get_mut(&c.contact_id).expect("checked");
                s.decrypt(&msg).map_err(|_| Fail::Skip(EventSkip::Undecryptable))?
            }
        };
        if plaintext.len() < 33 {
            return Err(Fail::Skip(EventSkip::Undecryptable));
        }
        let mut sender_ed = [0u8; 32];
        sender_ed.copy_from_slice(&plaintext[..32]);
        if sender_ed != peer_ed {
            contacts::note_presented_ed(&self.conn, &c.contact_id, &sender_ed)?;
            return Err(Fail::Skip(EventSkip::Mismatch));
        }
        let text =
            std::str::from_utf8(&plaintext[32..]).map_err(|_| Fail::Skip(EventSkip::Undecryptable))?;
        // ОДНА TX: пикл account (one-time consumed) + пикл сессии + inbox.
        let apickle =
            serde_json::to_string(&self.account.pickle()).map_err(|_| OlmError::Store("pickle".into()))?;
        let spickle = olm::pickle_session(self.sessions.get(&c.contact_id).expect("session"))?;
        let inserted = (|| -> Result<bool, OlmError> {
            let tx = self.conn.transaction().map_err(|e| OlmError::Store(format!("tx: {e}")))?;
            tx.execute(
                "INSERT INTO core_olm(id, pickle, next_key_id) VALUES(1,dmsg_seal('olm_pickle',?1),?2)
                 ON CONFLICT(id) DO UPDATE SET pickle=excluded.pickle",
                rusqlite::params![apickle, self.next_key_id],
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
            let n = tx
                .execute(
                    "INSERT INTO core_inbox(sender_device, message_id, contact_id, text, seq)
                      VALUES(?1,?2,?3,dmsg_seal('inbox_text',?4),?5)
                     ON CONFLICT(sender_device,message_id) DO NOTHING",
                    rusqlite::params![
                        e.sender.as_slice(),
                        e.message_id.as_slice(),
                        c.contact_id,
                        text,
                        e.seq as i64
                    ],
                )
                .map_err(|e| OlmError::Store(format!("inbox: {e}")))?;
            tx.commit().map_err(|e| OlmError::Store(format!("commit: {e}")))?;
            Ok(n == 1)
        })();
        match inserted {
            Err(e) => {
                self.sessions.remove(&c.contact_id);
                return Err(Fail::Err(e));
            }
            Ok(false) => Ok(None), // локальный дедуп: replay уже лежит
            Ok(true) => Ok(Some(Received {
                contact_id: c.contact_id.clone(),
                text: text.to_string(),
                message_id: e.message_id,
                seq: e.seq,
            })),
        }
    }

    /// Сессия для отправки: БД (сверка с пином) → claim + outbound.
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
            self.conn.execute(
                "INSERT INTO core_sessions(contact_id, pickle, peer_ed, peer_curve)
                 VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4)
                 ON CONFLICT(contact_id) DO NOTHING",
                rusqlite::params![c.contact_id, sp, peer_ed.as_slice(), peer_curve.as_slice()],
            ).map_err(|e| OlmError::Store(format!("session: {e}")))?;
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
        let mut payload = Vec::with_capacity(32 + wire.len());
        payload.extend_from_slice(user_id);
        payload.extend_from_slice(message_id);
        payload.extend_from_slice(wire);
        t.send_frame(OP_SEND, &payload)
            .await
            .map_err(|e| OlmError::Transport(e.to_string()))?;
        let (op, p) = t.recv_frame().await.map_err(|e| OlmError::Transport(e.to_string()))?;
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
            crate::store::outbox_set_status(&self.conn, message_id, status)
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

/// Точечный статус outbox (для классификации ретрая).
fn status_of(conn: &rusqlite::Connection, mid: &[u8; 16]) -> Result<String, OlmError> {
    conn.query_row(
        "SELECT status FROM core_outbox WHERE message_id=?1",
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

/// Маппинг ERROR для ENROL-replay (те же коды, что K2 enrol_from_qr).
fn map_enrol_error(code: u8) -> OlmError {
    use crate::enrol::EnrolError;
    use dmsg_protocol::{ERR_BAD, ERR_BOUND_OTHER, ERR_BUSY, ERR_EXPIRED, ERR_REVOKED};
    match code {
        c if c == ERR_BAD => OlmError::Enrol(EnrolError::Bad),
        c if c == ERR_EXPIRED => OlmError::Enrol(EnrolError::Expired),
        c if c == ERR_REVOKED => OlmError::Enrol(EnrolError::Revoked),
        c if c == ERR_BOUND_OTHER => OlmError::Enrol(EnrolError::BoundOther),
        c if c == ERR_BUSY => OlmError::Enrol(EnrolError::Busy),
        other => OlmError::Enrol(EnrolError::Server(other)),
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
    }

    impl Fake {
        fn new(replies: Vec<(u8, Vec<u8>)>) -> Self {
            Self { replies: replies.into(), echo_send_ack: false, sent: Vec::new() }
        }
        fn with_echo() -> Self {
            Self { replies: VecDeque::new(), echo_send_ack: true, sent: Vec::new() }
        }
    }

    impl Transport for Fake {
        async fn connect(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
        async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), TransportError> {
            self.sent.push((opcode, payload.to_vec()));
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
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
        async fn close(&mut self) {}
        fn is_connected(&self) -> bool {
            true
        }
    }

    fn tmp_core(name: &str) -> (Core, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmsg-k3t-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let db = dir.join("core.db");
        let _ = std::fs::remove_file(&db);
        // Enrol-заглушка: device_priv напрямую (без сети — как K2 store).
        let conn = crate::store::open(&db).expect("open");
        let privk = {
            let params: snow::params::NoiseParams =
                crate::transport::PATTERN.parse().expect("pattern");
            let kp = snow::Builder::new(params).generate_keypair().expect("keygen");
            let b: [u8; 32] = kp.private.as_slice().try_into().expect("len");
            b
        };
        crate::store::save_identity(&conn, &privk).expect("identity");
        crate::store::save_account(&conn, &[9u8; 16], "TESTCONTACT1").expect("account");
        crate::store::save_token(&conn, &[5u8; 32]).expect("token");
        drop(conn);
        (Core::open(&db).expect("core"), dir)
    }

    /// Связать два ядра контактами через настоящий QR-путь (без сети).
    fn link(a: &mut Core, b: &mut Core, aid: &str, bid: &str) {
        let (auid, _) = a.my_account().expect("a account");
        let (buid, _) = b.my_account().expect("b account");
        let (aed, acurve) = a.identity_keys();
        let (bed, bcurve) = b.identity_keys();
        let aqr =
            contacts::build_qr(aid, &auid, &a.device_pub(), &aed, &acurve).expect("aqr");
        let bqr =
            contacts::build_qr(bid, &buid, &b.device_pub(), &bed, &bcurve).expect("bqr");
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

    /// ENROLLED-reply для login-replay: user [9u8;16] + TESTCONTACT1.
    fn enrolled_resp() -> (u8, Vec<u8>) {
        let mut p = vec![9u8; 16];
        p.extend_from_slice(b"TESTCONTACT1");
        (OP_ENROLLED, p)
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
        let (mut a, da) = tmp_core(&format!("loop-a-{suffix}"));
        let (mut b, db) = tmp_core(&format!("loop-b-{suffix}"));
        if encrypted {
            drop(a);
            drop(b);
            a = Core::open_encrypted(&da.join("core.db"), &[9;32]).expect("sealed a");
            b = Core::open_encrypted(&db.join("core.db"), &[8;32]).expect("sealed b");
        }
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        // Настоящий one-time Боба для CLAIM-ответа (подписан его identity).
        b.account.generate_one_time_keys(1);
        let ot = *b.account.one_time_keys().values().next().expect("ot").as_bytes();
        let bdev = b.device_pub();
        let bsig = olm::sign_prekey(&b.account, &bdev, 5, &ot);
        let mut prekey = 5u32.to_be_bytes().to_vec();
        prekey.extend_from_slice(&ot);
        let _ = bsig; // подпись уже проверена сервером при upload; в CLAIM-ответе её нет
        let mut fa = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_PREKEY, prekey)]);
        fa.echo_send_ack = true;
        let mid = a.send_text(&mut fa, "BOBB00000002", "hello bob").await.expect("send");
        // Outbox: ciphertext сохранён, статус accepted.
        assert_eq!(status_of(&a.conn, &mid).expect("st"), crate::store::outbox_status::ACCEPTED);
        // Ретрай после accept: шлёт ТОТ ЖЕ ciphertext (сравниваем байты).
        let sent_ct = fa.sent.iter().find(|(op, _)| *op == OP_SEND).expect("send").1[32..].to_vec();
        if encrypted {
            drop(a);
            a = Core::open_encrypted(&da.join("core.db"), &[9;32]).expect("reopen a ratchet");
            assert_eq!(a.outbox_ciphertext(&mid).unwrap(), sent_ct);
            assert!(crate::store::load_session(&a.conn, "BOBB00000002").unwrap().is_some());
        }
        let mut fa2 = Fake::with_echo();
        fa2.replies.push_back(enrolled_resp());
        let stats = a.retry_queued(&mut fa2).await.expect("retry");
        assert_eq!((stats.resent, stats.skipped), (1, 0));
        let resent_ct = fa2.sent.iter().find(|(op, _)| *op == OP_SEND).expect("resend").1[32..].to_vec();
        assert_eq!(sent_ct, resent_ct, "retry must reuse saved ciphertext, not re-encrypt");
        // Приём Бобом: FETCH_RESP с серверным событием + ack cursor.
        let a_dev = a.device_pub();
        let mut fe = vec![0u8, 1];
        fe.extend_from_slice(&1u64.to_be_bytes());
        fe.extend_from_slice(&a_dev);
        fe.extend_from_slice(&mid);
        fe.extend_from_slice(&(sent_ct.len() as u16).to_be_bytes());
        fe.extend_from_slice(&sent_ct);
        let mut fb = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_FETCH_RESP, fe), (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec())]);
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
        assert!(b.sessions.is_empty());
        // Повтор той же пачки (reorder/replay): локальный дедуп — тишина.
        let mut fe2 = vec![0u8, 1];
        fe2.extend_from_slice(&1u64.to_be_bytes());
        fe2.extend_from_slice(&a_dev);
        fe2.extend_from_slice(&mid);
        fe2.extend_from_slice(&(sent_ct.len() as u16).to_be_bytes());
        fe2.extend_from_slice(&sent_ct);
        let mut fb2 = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_FETCH_RESP, fe2), (OP_DELIVERY_ACK, 1u64.to_be_bytes().to_vec())]);
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
            b.sessions.is_empty(),
            "durable replay must not load a ratchet"
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
        // The dedup key includes the sender; new/tampered events still fail.
        let mut event = RawEvent {
            seq: 2,
            sender: a_dev,
            message_id: [42; 16],
            ciphertext: sent_ct,
        };
        assert!(matches!(
            b.decrypt_event(&event).await,
            Err(Fail::Skip(EventSkip::Undecryptable))
        ));
        event.message_id = mid;
        event.sender = [42; 32];
        assert!(matches!(
            b.decrypt_event(&event).await,
            Err(Fail::Skip(EventSkip::Unknown))
        ));
        event.sender = a_dev;
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
        b.conn.execute("DROP TABLE core_inbox", []).unwrap();
        let mut fe3 = vec![0, 1];
        fe3.extend_from_slice(&1u64.to_be_bytes());
        fe3.extend_from_slice(&a_dev);
        fe3.extend_from_slice(&mid);
        fe3.extend_from_slice(&(ciphertext.len() as u16).to_be_bytes());
        fe3.extend_from_slice(&ciphertext);
        let mut fb3 = Fake::new(vec![
            enrolled_resp(),
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
        let mut fa = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_ERROR, vec![dmsg_protocol::ERR_NO_PREKEY])]);
        let r = a.send_text(&mut fa, "BOBB00000002", "hi").await;
        assert_eq!(r, Err(OlmError::NoPeerPrekeys));
        // Ничего не сохранено (шифровать было не для кого — нет сессии).
        assert!(crate::store::outbox_queued(&a.conn, 0, 32).expect("q").0.is_empty());
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn concurrent_offline_queue_advances_persisted_ratchet_once_per_message() {
        let (mut a, da) = tmp_core("offline-concurrent-a");
        let (mut b, db) = tmp_core("offline-concurrent-b");
        link(&mut a, &mut b, "ALICE0000001", "BOBB00000002");
        b.account.generate_one_time_keys(1);
        let ot = *b.account.one_time_keys().values().next().expect("ot").as_bytes();
        let (peer_ed, peer_curve) = b.identity_keys();
        let session = olm::outbound(&a.account, &peer_curve, &ot).expect("session");
        crate::store::save_session(&a.conn, "BOBB00000002", &olm::pickle_session(&session).expect("pickle"),
            &peer_ed, &peer_curve).expect("session store");
        drop(a);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for text in ["first offline", "second offline"] {
            let path = da.join("core.db");
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut core = Core::open(&path).expect("reopen");
                barrier.wait();
                core.queue_text_existing_session("BOBB00000002", text).expect("queued").expect("session")
            }));
        }
        let mids: Vec<_> = handles.into_iter().map(|h| h.join().expect("thread")).collect();
        assert_ne!(mids[0], mids[1]);
        let a = Core::open(&da.join("core.db")).expect("reopen after queue");
        let rows = crate::store::outbox_queued(&a.conn, 0, 32).expect("outbox").0;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.4 == crate::store::outbox_status::QUEUED));
        let mut batch = vec![0, 2];
        for (seq, (_, mid, _, ct, _)) in rows.iter().enumerate() {
            assert_eq!(a.outbox_ciphertext(mid).expect("ciphertext"), *ct);
            batch.extend_from_slice(&((seq + 1) as u64).to_be_bytes());
            batch.extend_from_slice(&a.device_pub());
            batch.extend_from_slice(mid);
            batch.extend_from_slice(&(ct.len() as u16).to_be_bytes());
            batch.extend_from_slice(ct);
        }
        let mut fake = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_FETCH_RESP, batch),
            (OP_DELIVERY_ACK, 2u64.to_be_bytes().to_vec())]);
        let result = b.fetch_and_decrypt(&mut fake).await.expect("decrypt in ratchet order");
        assert_eq!(result.received.len(), 2);
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
        let evil = contacts::build_qr("BOBB00000002", &buid, &b.device_pub(), &[9u8; 32], &[8u8; 32])
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
        // Явный confirm чинит отправку (дальше — ENROL + COUNT, скриптуем).
        contacts::confirm_identity(&a.conn, "BOBB00000002").expect("confirm");
        let mut fa = Fake::new(vec![enrolled_resp(), count_resp(16)]);
        // CLAIM вернёт реальный ключ НОВОГО боба? У тестового боба старые ключи —
        // подпись не сойдётся с новым пином... точнее: серверный путь здесь
        // пропущен (double), claim-ответ соберём от имени нового identity:
        // проще assert, что дело дошло до сети (COUNT съеден, дальше CLAIM).
        let r = a.send_text(&mut fa, "BOBB00000002", "hi").await;
        assert!(matches!(r, Err(OlmError::Transport(_))), "must reach network after confirm, got {r:?}");
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
        fe.extend_from_slice(&mid);
        let bad_ct = vec![9u8, 1, 2, 3];
        fe.extend_from_slice(&(bad_ct.len() as u16).to_be_bytes());
        fe.extend_from_slice(&bad_ct);
        let mut fb = Fake::new(vec![enrolled_resp(), count_resp(16), (OP_FETCH_RESP, fe), (OP_DELIVERY_ACK, 3u64.to_be_bytes().to_vec())]);
        let res = b.fetch_and_decrypt(&mut fb).await.expect("fetch");
        assert!(res.received.is_empty());
        assert_eq!(res.skipped_undecryptable, 1);
        assert_eq!(res.cursor, 3);
        // Кадр с неизвестной wire-версией — явная ошибка.
        let mut f = dmsg_protocol::encode_frame(OP_FETCH_RESP, b"x").expect("frame");
        f[0] = 9;
        assert_eq!(decode_app_frame(&f), Err(OlmError::WireVersion(9)));
        std::fs::remove_dir_all(&da).ok();
        std::fs::remove_dir_all(&db).ok();
    }
}
