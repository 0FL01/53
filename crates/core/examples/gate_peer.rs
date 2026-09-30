//! Throwaway direct-TCP or embedded DNS peer for device gates.
//! All bearer and message inputs are file paths; never print FFI errors or DTOs.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use dmsg_core::contacts;
use dmsg_core::ffi::{DmsgClient, QrKind, QrOutcome};
use dmsg_protocol::bootstrap;

type GateResult<T> = Result<T, &'static str>;

struct DnsGuard(std::sync::Arc<DmsgClient>);
impl Drop for DnsGuard {
    fn drop(&mut self) {
        let _ = self.0.stop_dns();
    }
}

fn dns_negative_profiles(
    invite: &Path,
    wrong_cert: &Path,
    wrong_pin: &Path,
    wrong_noise: &Path,
) -> GateResult<()> {
    let b = bootstrap::parse(&invite_file(invite)?).map_err(|_| "invalid invite")?;
    let wrong_cert = fs::read(wrong_cert).map_err(|_| "read public certificate fixture")?;
    for (output, cert, pubkey) in [
        (wrong_pin, wrong_cert.as_slice(), b.noise_pubkey),
        (wrong_noise, b.cert_der.as_slice(), [9u8; 32]),
    ] {
        let qr = bootstrap::build(&b.domain, cert, &pubkey, &[0u8; 32])
            .map_err(|_| "build public profile")?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(local_path(output)?)
            .map_err(|_| "create public profile")?;
        file.write_all(qr.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| "write public profile")?;
    }
    println!("public negative profiles ready (no bearer copied)");
    Ok(())
}

fn dns_provision(
    db: &Path,
    invite: &Path,
    resolvers: &Path,
    phone: &Path,
    output: &Path,
) -> GateResult<()> {
    let invite = invite_file(invite)?;
    let resolvers: Vec<String> = fs::read_to_string(resolvers)
        .map_err(|_| "read resolvers")?
        .lines()
        .map(str::to_string)
        .collect();
    dmsg_core::dns::parse_resolvers(resolvers.clone()).map_err(|_| "invalid resolvers")?;
    let phone = qr_file(phone)?;
    contacts::parse_qr(&phone).map_err(|_| "invalid phone contact qr")?;
    let output = local_path(output)?;
    if output.exists() || local_path(db)? == output {
        return Err("contact output exists/conflicts");
    }
    let db = db_file(db, true)?;
    let peer = client(&db)?;
    let _guard = DnsGuard(peer.clone());
    if peer.account_info().map_err(|_| "account")?.enrolled {
        return Err("peer already enrolled");
    }
    peer.enrol_dns(invite, resolvers)
        .map_err(|_| "DNS enrol failed")?;
    phone_contact(&peer, phone)?;
    let prekeys = peer.reconnect_dns().map_err(|_| "DNS reconnect failed")?;
    let qr = peer.my_contact_qr().map_err(|_| "contact qr")?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|_| "write contact qr")?;
    out.write_all(qr.as_bytes())
        .and_then(|_| out.sync_all())
        .map_err(|_| "write contact qr")?;
    println!("dns provision ok prekeys={prekeys}");
    Ok(())
}

fn dns_send(db: &Path, phone: &Path, message: &Path) -> GateResult<()> {
    let db = db_file(db, false)?;
    let peer = client(&db)?;
    let _guard = DnsGuard(peer.clone());
    let contact = phone_contact(&peer, qr_file(phone)?)?;
    let text = fs::read_to_string(message).map_err(|_| "read message")?;
    peer.send_dns(contact, text)
        .map_err(|_| "DNS send failed")?;
    println!("dns send ok");
    Ok(())
}

fn dns_fetch(db: &Path) -> GateResult<()> {
    let db = db_file(db, false)?;
    let peer = client(&db)?;
    let _guard = DnsGuard(peer.clone());
    let r = peer.fetch_dns().map_err(|_| "DNS fetch failed")?;
    println!(
        "dns fetch received={} unknown={} blocked={} undecryptable={} mismatch={}",
        r.received.len(),
        r.skipped_unknown,
        r.skipped_blocked,
        r.skipped_undecryptable,
        r.skipped_mismatch
    );
    Ok(())
}

struct Profile {
    addr: String,
    domain: String,
    server_pub: Vec<u8>,
}

fn nibble(b: u8) -> GateResult<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid transport profile"),
    }
}

fn parse_profile(raw: &str) -> GateResult<Profile> {
    let mut lines = raw.lines();
    let addr = lines.next().ok_or("invalid transport profile")?;
    let domain = lines.next().ok_or("invalid transport profile")?;
    let pub_hex = lines.next().ok_or("invalid transport profile")?;
    if lines.next().is_some()
        || addr.is_empty()
        || domain.is_empty()
        || addr.bytes().any(|b| b.is_ascii_whitespace())
        || domain.bytes().any(|b| b.is_ascii_whitespace())
        || pub_hex.len() != 64
    {
        return Err("invalid transport profile");
    }
    let mut server_pub = vec![0; 32];
    for (dst, pair) in server_pub
        .iter_mut()
        .zip(pub_hex.as_bytes().chunks_exact(2))
    {
        *dst = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(Profile {
        addr: addr.into(),
        domain: domain.into(),
        server_pub,
    })
}

fn profile(path: &Path) -> GateResult<Profile> {
    let raw = fs::read_to_string(path).map_err(|_| "read transport profile")?;
    parse_profile(&raw)
}

fn qr_file(path: &Path) -> GateResult<String> {
    let raw = fs::read_to_string(path).map_err(|_| "read qr file")?;
    Ok(raw.trim_end_matches(['\n', '\r']).to_owned())
}

fn invite_file(path: &Path) -> GateResult<String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "read invite file")?;
    if !meta.file_type().is_file() || meta.permissions().mode() & 0o777 != 0o600 {
        return Err("invite file must be 0600");
    }
    qr_file(path)
}

// Keep native plaintext SQLite and the exported QR inside this repo's ignored
// .local directory, even if the caller uses an absolute path or a symlinked parent.
fn local_path(path: &Path) -> GateResult<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid local path")?
        .join(".local")
        .canonicalize()
        .map_err(|_| "invalid local path")?;
    let parent = path
        .parent()
        .ok_or("invalid local path")?
        .canonicalize()
        .map_err(|_| "invalid local path")?;
    if !parent.starts_with(root) {
        return Err("invalid local path");
    }
    Ok(parent.join(path.file_name().ok_or("invalid local path")?))
}

fn db_file(path: &Path, create: bool) -> GateResult<PathBuf> {
    let path = local_path(path)?;
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|_| "peer database permissions")?;
        }
        Ok(_) => return Err("invalid peer database"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|_| "create peer database")?;
        }
        Err(_) => return Err("peer database missing"),
    }
    Ok(path)
}

fn client(path: &Path) -> GateResult<std::sync::Arc<DmsgClient>> {
    Ok(DmsgClient::open(
        path.to_str().ok_or("invalid peer database path")?.into(),
    ))
}

fn phone_contact(peer: &DmsgClient, qr: String) -> GateResult<String> {
    let contact_id = contacts::parse_qr(&qr)
        .map_err(|_| "invalid phone contact qr")?
        .contact_id;
    if peer.add_contact_qr(qr).map_err(|_| "add phone contact")? == QrOutcome::IdentityChanged {
        return Err("phone identity changed");
    }
    let info = peer
        .contact_get(contact_id.clone())
        .map_err(|_| "verify phone contact")?;
    if info.identity_mismatch || !info.has_keys {
        return Err("phone identity changed");
    }
    peer.contact_accept(contact_id.clone())
        .map_err(|_| "accept phone contact")?;
    Ok(contact_id)
}

fn provision(
    db: &Path,
    invite: &Path,
    transport: &Path,
    phone: &Path,
    output: &Path,
) -> GateResult<()> {
    let profile = profile(transport)?;
    let invite = invite_file(invite)?;
    if dmsg_core::ffi::qr_kind(invite.clone()).map_err(|_| "invalid invite")? != QrKind::Join {
        return Err("invalid invite");
    }
    let bootstrap = bootstrap::parse(&invite).map_err(|_| "invalid invite")?;
    if bootstrap.domain != profile.domain.as_bytes()
        || bootstrap.noise_pubkey.as_slice() != profile.server_pub.as_slice()
    {
        return Err("transport profile does not match invite");
    }
    let phone = qr_file(phone)?;
    contacts::parse_qr(&phone).map_err(|_| "invalid phone contact qr")?;
    let output = local_path(output)?;
    match fs::symlink_metadata(&output) {
        Ok(_) => return Err("contact qr output exists"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("contact qr output unavailable"),
    }
    let db_path = local_path(db)?;
    if db_path == output {
        return Err("contact qr output conflicts with database");
    }
    db_path.to_str().ok_or("invalid peer database path")?;
    let db = db_file(db, true)?;
    let peer = client(&db)?;
    if peer
        .account_info()
        .map_err(|_| "read peer account")?
        .enrolled
    {
        return Err("peer already enrolled");
    }
    peer.enrol_from_qr(invite, profile.addr.clone(), None)
        .map_err(|_| "enrolment failed")?;
    phone_contact(&peer, phone)?;
    let prekeys = peer
        .reconnect(profile.addr, profile.server_pub, profile.domain)
        .map_err(|_| "reconnect failed")?;
    let own_qr = peer.my_contact_qr().map_err(|_| "build contact qr")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|_| "write contact qr")?;
    file.write_all(own_qr.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "write contact qr")?;
    println!("provision ok prekeys={prekeys}");
    Ok(())
}

fn send(db: &Path, transport: &Path, phone: &Path, message: &Path) -> GateResult<()> {
    let db = db_file(db, false)?;
    let profile = profile(transport)?;
    let qr = qr_file(phone)?;
    let text = fs::read_to_string(message).map_err(|_| "read message file")?;
    let peer = client(&db)?;
    if !peer
        .account_info()
        .map_err(|_| "read peer account")?
        .enrolled
    {
        return Err("peer not enrolled");
    }
    let contact_id = phone_contact(&peer, qr)?;
    peer.send_text(
        profile.addr,
        profile.server_pub,
        profile.domain,
        contact_id,
        text,
    )
    .map_err(|_| "send failed")?;
    println!("send ok");
    Ok(())
}

fn fetch(db: &Path, transport: &Path) -> GateResult<()> {
    let db = db_file(db, false)?;
    let profile = profile(transport)?;
    let peer = client(&db)?;
    if !peer
        .account_info()
        .map_err(|_| "read peer account")?
        .enrolled
    {
        return Err("peer not enrolled");
    }
    let report = peer
        .fetch(profile.addr, profile.server_pub, profile.domain)
        .map_err(|_| "fetch failed")?;
    println!(
        "fetch received={} skipped_unknown={} skipped_blocked={} skipped_undecryptable={} skipped_mismatch={}",
        report.received.len(),
        report.skipped_unknown,
        report.skipped_blocked,
        report.skipped_undecryptable,
        report.skipped_mismatch
    );
    Ok(())
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let result = match args.get(1).and_then(|s| s.to_str()) {
        Some("dns-negative-profiles") if args.len() == 6 => dns_negative_profiles(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
            Path::new(&args[5]),
        ),
        Some("dns-provision") if args.len() == 7 => dns_provision(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
            Path::new(&args[5]),
            Path::new(&args[6]),
        ),
        Some("dns-send") if args.len() == 5 => dns_send(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        ),
        Some("dns-fetch") if args.len() == 3 => dns_fetch(Path::new(&args[2])),
        Some("provision") if args.len() == 7 => provision(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
            Path::new(&args[5]),
            Path::new(&args[6]),
        ),
        Some("send") if args.len() == 6 => send(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
            Path::new(&args[5]),
        ),
        Some("fetch") if args.len() == 4 => fetch(Path::new(&args[2]), Path::new(&args[3])),
        _ => {
            eprintln!("usage: gate_peer provision <peer-db-path> <invite-path> <transport-profile-path> <phone-public-contact-qr-path> <peer-public-contact-qr-output-path> | send <peer-db-path> <profile-path> <phone-public-contact-qr-path> <message-file-path> | fetch <peer-db-path> <profile-path>");
            eprintln!("DNS: dns-provision <db> <invite> <numeric-resolvers-file> <phone-contact-qr> <peer-contact-output> | dns-send <db> <phone-contact-qr> <message-file> | dns-fetch <db>");
            std::process::exit(2);
        }
    };
    if let Err(category) = result {
        eprintln!("gate_peer: {category}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_requires_three_lines_and_exact_hex_key() {
        let raw = format!("127.0.0.1:17000\nmsg.example\n{}\n", "aB".repeat(32));
        let p = parse_profile(&raw).expect("profile");
        assert_eq!(p.server_pub, vec![0xab; 32]);
        assert!(parse_profile(&(raw.clone() + "extra\n")).is_err());
        assert!(parse_profile(&raw.replace('B', "Z")).is_err());
        assert!(parse_profile(&raw.replace("aB", "ab").replace("ab", "a")).is_err());
    }
}
