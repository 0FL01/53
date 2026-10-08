//! Disposable fresh-schema DNS voice peer. All credentials and storage keys are
//! read from owner-only files; diagnostics contain only static categories/counts.
//! No endpoint defaults, audio files, hashes, or direct-TCP transport override.
use dmsg_core::{
    contacts,
    ffi::{
        DeliveryState, DmsgClient, FetchReport, FfiError, HistoryMessage, MessageDirection,
        MessageKind, QrOutcome, RetryReport,
    },
    voice_codec::{VoiceDecoder, VoiceEncoder, VoiceNote, MAX_BATCH_SAMPLES, SAMPLE_RATE},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{mpsc, Arc},
    time::Duration,
};

const USAGE: &str = "usage:
  voice_gate_peer signup DB KEY_FILE FIXTURE_JSON OWN_QR_OUTPUT
  voice_gate_peer accept DB KEY_FILE FIXTURE_JSON PHONE_QR_FILE
  voice_gate_peer fetch DB KEY_FILE FIXTURE_JSON
  voice_gate_peer send-voice DB KEY_FILE FIXTURE_JSON PHONE_QR_FILE PROOF_OUTPUT
  voice_gate_peer download DB KEY_FILE FIXTURE_JSON PHONE_QR_FILE
  voice_gate_peer verify DB KEY_FILE FIXTURE_JSON PHONE_QR_FILE [EXPECTED_VISIBLE_VOICE [EXPECTED_DELETED_VOICE]]
  voice_gate_peer --help

FIXTURE_JSON: owner-only JSON with serverCode, resolvers (numeric IP:port),
login, password, invitation (string or null). KEY_FILE is exactly 32 raw bytes.
signup requires a fresh DB path and creates a 0600 key if absent.
Outputs are new 0600 files. Proof contains only public IDs/size/sample counts.
History is bounded to 1000 rows; each transfer is bounded to 32 steps.
All network operations use the explicitly configured native DNS carrier.";

const MAX_FIXTURE_BYTES: usize = 32 * 1024;
const MAX_HISTORY_PAGES: usize = 10;
const TRANSFER_STEPS: u32 = 32;
const NOTE_SAMPLES: u32 = 12 * SAMPLE_RATE;
const ACTION_TIMEOUT: Duration = Duration::from_secs(600);

struct Failure {
    stage: &'static str,
    category: &'static str,
}
type Result<T> = std::result::Result<T, Failure>;

fn fail(stage: &'static str, category: &'static str) -> Failure {
    Failure { stage, category }
}

fn core<T>(stage: &'static str, result: std::result::Result<T, FfiError>) -> Result<T> {
    result.map_err(|error| {
        let category = match error {
            FfiError::BadArgs(_) | FfiError::InvalidInput | FfiError::BadText => "invalid_input",
            FfiError::BadQr(_) => "invalid_qr",
            FfiError::PinMismatch => "pin_mismatch",
            FfiError::NotEnrolled => "not_enrolled",
            FfiError::UnknownContact => "unknown_contact",
            FfiError::NotAccepted => "not_accepted",
            FfiError::Blocked => "blocked",
            FfiError::IdentityMismatch => "identity_mismatch",
            FfiError::NothingToConfirm => "nothing_to_confirm",
            FfiError::MissingKeys => "missing_keys",
            FfiError::NoPeerPrekeys => "no_peer_prekeys",
            FfiError::UploadRejected => "upload_rejected",
            FfiError::Quota => "quota",
            FfiError::Revoked => "revoked",
            FfiError::InvalidCredentials => "invalid_credentials",
            FfiError::LoginTaken => "login_taken",
            FfiError::InviteRequired => "invite_required",
            FfiError::InviteExpired => "invite_expired",
            FfiError::InviteRevoked => "invite_revoked",
            FfiError::InviteUsed => "invite_used",
            FfiError::AuthRateLimited => "auth_rate_limited",
            FfiError::Busy => "busy",
            FfiError::MessageChanged => "message_changed",
            FfiError::MessageUnavailable => "message_unavailable",
            FfiError::VoiceSessionRequired => "voice_session_required",
            FfiError::BadVoice => "bad_voice",
            FfiError::Transport(reason) => {
                if reason.contains("timeout") || reason.contains("deadline") {
                    "timeout"
                } else {
                    "transport"
                }
            }
            FfiError::Store(_) => "store",
            FfiError::Crypto(_) => "crypto",
            FfiError::Protocol(_) => "protocol",
            FfiError::Server(_) => "server",
        };
        fail(stage, category)
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[serde(rename = "serverCode")]
    server_code: String,
    resolvers: Vec<String>,
    login: String,
    password: String,
    invitation: Option<String>,
}

fn private_metadata(path: &Path, stage: &'static str) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path).map_err(|_| fail(stage, "file_unavailable"))?;
    let mode = meta.permissions().mode() & 0o777;
    if !meta.is_file() || !matches!(mode, 0o400 | 0o600) {
        return Err(fail(stage, "owner_only_regular_file_required"));
    }
    Ok(meta)
}

fn read_file(path: &Path, private: bool, limit: usize, stage: &'static str) -> Result<Vec<u8>> {
    let before = if private {
        private_metadata(path, stage)?
    } else {
        fs::symlink_metadata(path).map_err(|_| fail(stage, "file_unavailable"))?
    };
    if !before.is_file() || before.len() > limit as u64 {
        return Err(fail(stage, "invalid_file"));
    }
    let file = File::open(path).map_err(|_| fail(stage, "file_unreadable"))?;
    let after = file
        .metadata()
        .map_err(|_| fail(stage, "file_unreadable"))?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mode() != after.mode()
        || !after.is_file()
    {
        return Err(fail(stage, "file_changed"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail(stage, "file_unreadable"))?;
    if bytes.len() > limit {
        return Err(fail(stage, "file_too_large"));
    }
    Ok(bytes)
}

fn fixture(path: &Path) -> Result<Fixture> {
    let bytes = read_file(path, true, MAX_FIXTURE_BYTES, "fixture")?;
    let fixture: Fixture =
        serde_json::from_slice(&bytes).map_err(|_| fail("fixture", "invalid_json"))?;
    dmsg_core::dns::Profile::from_qr(&fixture.server_code, fixture.resolvers.clone())
        .map_err(|_| fail("fixture", "invalid_profile_or_resolvers"))?;
    let invitation = fixture
        .invitation
        .as_deref()
        .map(dmsg_protocol::auth::parse_invitation)
        .transpose()
        .map_err(|_| fail("fixture", "invalid_invitation"))?;
    dmsg_protocol::auth::build_signup(&fixture.login, &fixture.password, invitation.as_ref())
        .map_err(|_| fail("fixture", "invalid_credentials"))?;
    Ok(fixture)
}

fn new_output(path: &Path, stage: &'static str) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| fail(stage, "output_exists_or_unavailable"))
}

fn write_output(file: &mut File, bytes: &[u8], stage: &'static str) -> Result<()> {
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| fail(stage, "output_write"))
}

fn storage_key(path: &Path, create: bool) -> Result<[u8; 32]> {
    match fs::symlink_metadata(path) {
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0; 32];
            getrandom::fill(&mut key).map_err(|_| fail("key", "random_unavailable"))?;
            write_output(&mut new_output(path, "key")?, &key, "key")?;
            Ok(key)
        }
        _ => read_file(path, true, 32, "key")?
            .try_into()
            .map_err(|_| fail("key", "expected_32_raw_bytes")),
    }
}

struct DnsGuard(Arc<DmsgClient>);
impl Drop for DnsGuard {
    fn drop(&mut self) {
        let _ = self.0.stop_dns();
    }
}

fn peer_qr(path: &Path) -> Result<(String, String)> {
    let bytes = read_file(path, false, dmsg_core::ffi::QR_URI_MAX, "peer_qr")?;
    let qr = String::from_utf8(bytes).map_err(|_| fail("peer_qr", "invalid_qr"))?;
    let qr = qr.trim_end_matches(['\r', '\n']).to_owned();
    let cid = contacts::parse_qr(&qr)
        .map_err(|_| fail("peer_qr", "invalid_qr"))?
        .contact_id;
    Ok((qr, cid))
}

fn checked_contact(client: &DmsgClient, qr: String, cid: &str) -> Result<()> {
    if core("contact_pin", client.add_contact_qr(qr))? == QrOutcome::IdentityChanged {
        return Err(fail("contact_pin", "identity_mismatch"));
    }
    let contact = core("contact_pin", client.contact_get(cid.into()))?;
    if contact.identity_mismatch || !contact.has_keys {
        return Err(fail("contact_pin", "identity_mismatch_or_missing_keys"));
    }
    if !matches!(
        contact.state.as_str(),
        contacts::state::ACCEPTED | contacts::state::ACCEPTED_SERVER
    ) {
        return Err(fail("contact_pin", "not_accepted"));
    }
    Ok(())
}

fn history(client: &DmsgClient, cid: &str) -> Result<Vec<HistoryMessage>> {
    let mut rows = Vec::new();
    let mut cursor = None;
    for _ in 0..MAX_HISTORY_PAGES {
        let page = core("history", client.history_page(cid.into(), cursor, 100))?;
        rows.extend(page.rows);
        match page.next_before_local_id {
            None => return Ok(rows),
            Some(next) if cursor.is_none_or(|old| next < old) => cursor = Some(next),
            _ => return Err(fail("history", "invalid_cursor")),
        }
    }
    Err(fail("history", "row_limit"))
}

fn note() -> Result<VoiceNote> {
    let mut encoder = VoiceEncoder::new().map_err(|_| fail("encode", "codec"))?;
    for batch in 0..120 {
        let pcm: Vec<i16> = (0..MAX_BATCH_SAMPLES)
            .map(|i| {
                let t = (batch * MAX_BATCH_SAMPLES + i) as f64 / SAMPLE_RATE as f64;
                let phase = t * std::f64::consts::TAU;
                (8000.0 * (phase * 180.0).sin()
                    + 2000.0 * (phase * 360.0).sin()
                    + 1000.0 * (phase * 540.0).sin()) as i16
            })
            .collect();
        encoder.push(&pcm).map_err(|_| fail("encode", "codec"))?;
    }
    let note = encoder.finish().map_err(|_| fail("encode", "codec"))?;
    if note.sample_count != NOTE_SAMPLES || note.bytes.len() <= 8192 {
        return Err(fail("encode", "multi_chunk_note_required"));
    }
    Ok(note)
}

fn transfer(client: &DmsgClient, cid: &str, local_id: i64, download: bool) -> Result<u32> {
    let handle = core(
        "transfer_prepare",
        client.prepare_voice_transfer(cid.into(), local_id, download),
    )?;
    let result = (|| {
        for step in 1..=TRANSFER_STEPS {
            let advanced = core("transfer_advance", handle.advance())?;
            let committed = core(
                "transfer_commit",
                client.commit_voice_transfer(handle.clone()),
            )?;
            if advanced != committed || committed.transferred > committed.total {
                return Err(fail("transfer_commit", "progress_mismatch"));
            }
            if committed.complete {
                if committed.transferred != committed.total {
                    return Err(fail("transfer_commit", "incomplete_bytes"));
                }
                return Ok(step);
            }
        }
        Err(fail("transfer_advance", "step_limit"))
    })();
    handle.cancel();
    result
}

fn fetch_counts(report: &FetchReport) -> Value {
    json!({
        "received_text": report.received.iter().filter(|r| r.kind == MessageKind::Text).count(),
        "received_voice": report.received.iter().filter(|r| r.kind == MessageKind::Voice).count(),
        "skipped_unknown": report.skipped_unknown, "skipped_blocked": report.skipped_blocked,
        "skipped_undecryptable": report.skipped_undecryptable, "skipped_mismatch": report.skipped_mismatch,
        "cursor": report.cursor,
    })
}

fn retry_counts(report: &RetryReport) -> Value {
    json!({"resent": report.resent, "accepted": report.accepted,
           "delivered": report.delivered, "skipped": report.skipped})
}

fn print_report(report: Value) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &report).map_err(|_| fail("report", "output_write"))?;
    output
        .write_all(b"\n")
        .map_err(|_| fail("report", "output_write"))
}

fn expected_count(value: Option<&String>) -> Result<Option<usize>> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| fail("arguments", "invalid_expected_count"))
        })
        .transpose()
}

fn valid_args(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("signup" | "accept" | "download") => args.len() == 5,
        Some("fetch") => args.len() == 4,
        Some("send-voice") => args.len() == 6,
        Some("verify") => (5..=7).contains(&args.len()),
        _ => false,
    }
}

fn run(args: &[String]) -> Result<()> {
    let action = args[0].as_str();
    let expected_visible = if action == "verify" {
        expected_count(args.get(5))?
    } else {
        None
    };
    let expected_deleted = if action == "verify" {
        expected_count(args.get(6))?
    } else {
        None
    };
    let fixture = fixture(Path::new(&args[3]))?;
    let db = Path::new(&args[1]);
    let signup = action == "signup";
    if signup {
        match fs::symlink_metadata(db) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(fail("db", "fresh_path_required")),
        }
    } else {
        private_metadata(db, "db")?;
    }
    let key = storage_key(Path::new(&args[2]), signup)?;
    // Reserve a fresh owner-only empty DB; core initializes fresh schema10.
    // Existing databases and secret inputs are never truncated or replaced.
    if signup {
        new_output(db, "db")?
            .sync_all()
            .map_err(|_| fail("db", "output_write"))?;
    }
    let client = core(
        "open",
        DmsgClient::open_encrypted(args[1].clone(), key.to_vec()),
    )?;
    let _guard = DnsGuard(client.clone());
    core(
        "configure_dns",
        client.configure_dns(fixture.server_code, fixture.resolvers),
    )?;
    match action {
        "signup" => {
            let mut output = new_output(Path::new(&args[4]), "own_qr_output")?;
            let account = core(
                "signup_dns",
                client.signup_dns(fixture.login, fixture.password, fixture.invitation),
            )?;
            if !account.authenticated {
                return Err(fail("signup_dns", "not_enrolled"));
            }
            let prekeys = core("reconnect_dns", client.reconnect_dns())?;
            let own_qr = core("own_qr", client.my_contact_qr())?;
            write_output(&mut output, own_qr.as_bytes(), "own_qr_output")?;
            print_report(
                json!({"action":"signup", "ok":1, "authenticated":1, "prekeys":prekeys, "public_qr_files":1}),
            )
        }
        "accept" => {
            let (qr, cid) = peer_qr(Path::new(&args[4]))?;
            if core("invite_contact_qr", client.invite_contact_qr(qr.clone()))?
                == QrOutcome::IdentityChanged
            {
                return Err(fail("invite_contact_qr", "identity_mismatch"));
            }
            let prekeys = core("reconnect_dns", client.reconnect_dns())?;
            let fetched = core("fetch_dns", client.fetch_dns())?;
            checked_contact(&client, qr, &cid)?;
            print_report(
                json!({"action":"accept", "ok":1, "accepted_contacts":1, "prekeys":prekeys, "fetch":fetch_counts(&fetched)}),
            )
        }
        "fetch" => {
            let fetched = core("fetch_dns", client.fetch_dns())?;
            let retried = core("retry_dns", client.retry_dns())?;
            print_report(
                json!({"action":"fetch", "ok":1, "fetch":fetch_counts(&fetched), "retry":retry_counts(&retried)}),
            )
        }
        "send-voice" => {
            let (qr, cid) = peer_qr(Path::new(&args[4]))?;
            checked_contact(&client, qr, &cid)?;
            let mut output = new_output(Path::new(&args[5]), "proof_output")?;
            let note = note()?;
            if !core(
                "voice_session_ready",
                client.voice_session_ready(cid.clone()),
            )? {
                core(
                    "prime_voice_session_dns",
                    client.prime_voice_session_dns(cid.clone()),
                )?;
            }
            let mut mid = [0; 16];
            getrandom::fill(&mut mid).map_err(|_| fail("mid", "random_unavailable"))?;
            let mid_hex: String = mid.iter().map(|b| format!("{b:02x}")).collect();
            let encoded_bytes = note.bytes.len();
            let queued = core(
                "queue_voice",
                client.queue_voice(cid.clone(), mid_hex, note.bytes, None),
            )?;
            let voice = queued
                .voice
                .as_ref()
                .ok_or_else(|| fail("queue_voice", "missing_voice_metadata"))?;
            let mut proof = json!({"message_id_hex":queued.message_id_hex, "local_id":queued.local_id,
                "encoded_bytes":encoded_bytes, "byte_len":voice.byte_len, "sample_count":voice.sample_count,
                "upload_steps":0, "upload_complete":false});
            // Persist the public MID immediately so a failed/terminated transfer
            // still has an exact durable queue identifier, without audio output.
            write_output(
                &mut output,
                &serde_json::to_vec(&proof).map_err(|_| fail("proof_output", "encode"))?,
                "proof_output",
            )?;
            let steps = transfer(&client, &cid, queued.local_id, false)?;
            let retried = core("retry_dns", client.retry_dns())?;
            let sent = core(
                "history_message",
                client.history_message(cid, queued.local_id),
            )?;
            if !matches!(
                sent.delivery_state,
                Some(DeliveryState::Accepted | DeliveryState::Delivered)
            ) || sent.server_seq.is_none()
                || sent.server_timestamp_ms.is_none()
            {
                return Err(fail("retry_dns", "voice_not_accepted"));
            }
            proof["upload_steps"] = json!(steps);
            proof["upload_complete"] = json!(true);
            proof["server_seq"] = json!(sent.server_seq);
            proof["server_timestamp_ms"] = json!(sent.server_timestamp_ms);
            output
                .set_len(0)
                .and_then(|_| output.seek(SeekFrom::Start(0)))
                .map_err(|_| fail("proof_output", "output_write"))?;
            write_output(
                &mut output,
                &serde_json::to_vec(&proof).map_err(|_| fail("proof_output", "encode"))?,
                "proof_output",
            )?;
            print_report(
                json!({"action":"send-voice", "ok":1, "queued_voice":1, "upload_complete":1,
                "upload_steps":steps, "encoded_bytes":encoded_bytes, "byte_len":voice.byte_len,
                "sample_count":voice.sample_count, "public_proof_files":1, "retry":retry_counts(&retried)}),
            )
        }
        "download" => {
            let (qr, cid) = peer_qr(Path::new(&args[4]))?;
            checked_contact(&client, qr, &cid)?;
            let fetched = core("fetch_dns", client.fetch_dns())?;
            let rows = history(&client, &cid)?;
            let mut incoming_voice = 0;
            let mut downloads = 0;
            let mut steps = 0;
            let mut sample_count = 0u64;
            let mut encoded_bytes = 0usize;
            for row in rows {
                if row.kind != MessageKind::Voice
                    || row.direction != MessageDirection::Incoming
                    || row.hidden_self
                    || row.deleted_all
                {
                    continue;
                }
                incoming_voice += 1;
                let metadata = row
                    .voice
                    .ok_or_else(|| fail("download", "missing_voice_metadata"))?;
                if !metadata.downloaded {
                    steps += transfer(&client, &cid, row.local_id, true)?;
                    downloads += 1;
                }
                let bytes = core("voice_data", client.voice_data(cid.clone(), row.local_id))?;
                let mut decoder = VoiceDecoder::new(&bytes).map_err(|_| fail("decode", "codec"))?;
                let mut decoded = 0u32;
                for _ in 0..=600 {
                    let pcm = decoder
                        .read(MAX_BATCH_SAMPLES)
                        .map_err(|_| fail("decode", "codec"))?;
                    if pcm.is_empty() {
                        break;
                    }
                    decoded += pcm.len() as u32;
                }
                if decoded != metadata.sample_count {
                    return Err(fail("decode", "sample_count_mismatch"));
                }
                sample_count += decoded as u64;
                encoded_bytes += bytes.len();
            }
            print_report(
                json!({"action":"download", "ok":1, "incoming_voice":incoming_voice,
                "downloaded_voice":downloads, "decoded_voice":incoming_voice, "transfer_steps":steps,
                "sample_count":sample_count, "encoded_bytes":encoded_bytes, "fetch":fetch_counts(&fetched)}),
            )
        }
        "verify" => {
            let (qr, cid) = peer_qr(Path::new(&args[4]))?;
            checked_contact(&client, qr, &cid)?;
            let rows = history(&client, &cid)?;
            let voices: Vec<_> = rows
                .iter()
                .filter(|r| r.kind == MessageKind::Voice)
                .collect();
            let visible = voices
                .iter()
                .filter(|r| !r.hidden_self && !r.deleted_all)
                .count();
            let hidden = voices.iter().filter(|r| r.hidden_self).count();
            let deleted = voices.iter().filter(|r| r.deleted_all).count();
            if expected_visible.is_some_and(|n| n != visible)
                || expected_deleted.is_some_and(|n| n != deleted)
            {
                return Err(fail("verify", "expected_count_mismatch"));
            }
            let mut order = Vec::new();
            for row in &voices {
                match (row.server_seq, row.server_timestamp_ms) {
                    (Some(seq), Some(time)) if seq > 0 && time >= 0 => (),
                    (None, None) if row.delivery_state == Some(DeliveryState::Queued) => (),
                    _ => return Err(fail("verify", "invalid_original_order")),
                }
                order.push(json!({"local_id":row.local_id, "server_seq":row.server_seq,
                                  "server_timestamp_ms":row.server_timestamp_ms}));
            }
            order.sort_by_key(|r| {
                (
                    r["server_seq"].as_i64().unwrap_or(i64::MAX),
                    r["local_id"].as_i64(),
                )
            });
            print_report(
                json!({"action":"verify", "ok":1, "history_rows":rows.len(), "voice_rows":voices.len(),
                "visible_voice":visible, "hidden_voice":hidden, "deleted_voice":deleted,
                "incoming_voice":voices.iter().filter(|r| r.direction == MessageDirection::Incoming).count(),
                "queued_voice":voices.iter().filter(|r| r.delivery_state == Some(DeliveryState::Queued)).count(),
                "accepted_voice":voices.iter().filter(|r| r.delivery_state == Some(DeliveryState::Accepted)).count(),
                "delivered_voice":voices.iter().filter(|r| r.delivery_state == Some(DeliveryState::Delivered)).count(),
                "original_voice_order":order}),
            )
        }
        _ => Err(fail("arguments", "usage")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "help" | "-h") {
        println!("{USAGE}");
        return;
    }
    if !valid_args(&args) {
        eprintln!("voice_gate_peer: stage=arguments category=usage\n{USAGE}");
        std::process::exit(2);
    }
    // DNS facade commands are blocking. Bound the whole disposable process too,
    // including auth/fetch/retry, without logging errors or private inputs.
    let (done, wait) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if wait.recv_timeout(ACTION_TIMEOUT) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("voice_gate_peer: stage=action category=timeout");
            std::process::exit(1);
        }
    });
    let result = run(&args);
    let _ = done.send(());
    if let Err(error) = result {
        eprintln!(
            "voice_gate_peer: stage={} category={}",
            error.stage, error.category
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_note_spans_chunks_and_decodes_exactly_without_audio_files() {
        let note = note().unwrap_or_else(|_| panic!("fixed note failed"));
        assert!(note.bytes.len() > 8192);
        assert_eq!(note.sample_count, NOTE_SAMPLES);
        let mut decoder = VoiceDecoder::new(&note.bytes).unwrap();
        let mut samples = 0;
        let mut audible = false;
        loop {
            let pcm = decoder.read(MAX_BATCH_SAMPLES).unwrap();
            if pcm.is_empty() {
                break;
            }
            audible |= pcm.iter().any(|v| v.unsigned_abs() > 100);
            samples += pcm.len() as u32;
        }
        assert_eq!(samples, NOTE_SAMPLES);
        assert!(audible);
    }

    #[test]
    fn required_arguments_are_checked_without_opening_files() {
        for action in [
            "signup",
            "accept",
            "fetch",
            "send-voice",
            "download",
            "verify",
        ] {
            assert!(!valid_args(&[action.into()]));
        }
        assert!(valid_args(
            &["fetch", "db", "key", "fixture"].map(str::to_owned)
        ));
        assert!(!valid_args(
            &["other", "db", "key", "fixture"].map(str::to_owned)
        ));
        assert!(expected_count(Some(&"1".into())).is_ok());
        assert!(expected_count(Some(&"not-a-count".into())).is_err());
    }
}
