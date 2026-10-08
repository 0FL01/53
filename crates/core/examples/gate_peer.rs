//! Disposable DNS peer. Credentials/invitation/message inputs are files, never
//! argv or diagnostics. Public profile and invitation are separate inputs.
use dmsg_core::{
    contacts,
    ffi::{DmsgClient, QrOutcome},
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
type Result<T> = std::result::Result<T, &'static str>;
struct Guard(Arc<DmsgClient>);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.dns_stop();
    }
}
fn text(path: &Path, secret: bool) -> Result<String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "input unavailable")?;
    if !meta.is_file() || (secret && meta.permissions().mode() & 0o777 != 0o600) {
        return Err("secret input must be a regular 0600 file");
    }
    fs::read_to_string(path).map_err(|_| "input unreadable")
}
fn local(path: &Path) -> Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local")
        .canonicalize()
        .map_err(|_| "local root unavailable")?;
    let parent = path
        .parent()
        .ok_or("invalid path")?
        .canonicalize()
        .map_err(|_| "invalid parent")?;
    if !parent.starts_with(root) {
        return Err("gate outputs must be under .local");
    }
    Ok(parent.join(path.file_name().ok_or("invalid filename")?))
}
fn client(path: &Path) -> Result<Arc<DmsgClient>> {
    let path = local(path)?;
    Ok(DmsgClient::open(
        path.to_str().ok_or("invalid db path")?.into(),
    ))
}
fn peer_contact(client: &DmsgClient, path: &Path) -> Result<String> {
    let qr = text(path, false)?.trim_end_matches(['\r', '\n']).to_owned();
    let id = contacts::parse_qr(&qr)
        .map_err(|_| "invalid contact")?
        .contact_id;
    if client.add_contact_qr(qr).map_err(|_| "contact import")? == QrOutcome::IdentityChanged {
        return Err("peer identity changed");
    }
    if client
        .contact_get(id.clone())
        .map_err(|_| "contact read")?
        .identity_mismatch
    {
        return Err("peer identity changed");
    }
    client
        .contact_accept(id.clone())
        .map_err(|_| "contact accept")?;
    Ok(id)
}
fn provision(args: &[String]) -> Result<()> {
    let client = client(Path::new(&args[0]))?;
    let _guard = Guard(client.clone());
    let code = text(Path::new(&args[1]), false)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    let resolvers = text(Path::new(&args[2]), false)?
        .lines()
        .map(str::to_owned)
        .collect();
    client
        .configure_dns(code, resolvers)
        .map_err(|_| "invalid profile/resolvers")?;
    // Credential file has login, password, optional standalone invitation.
    // Password whitespace is significant and is never trimmed.
    let credentials = text(Path::new(&args[3]), true)?;
    let (login, password, invite) = parse_credentials(&credentials)?;
    client
        .signup_dns(login.into(), password.into(), invite.map(str::to_owned))
        .map_err(|_| "DNS signup failed")?;
    peer_contact(&client, Path::new(&args[4]))?;
    let count = client.reconnect_dns().map_err(|_| "DNS reconnect failed")?;
    let qr = client.my_contact_qr().map_err(|_| "contact export")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(local(Path::new(&args[5]))?)
        .map_err(|_| "output exists/unavailable")?;
    output
        .write_all(qr.as_bytes())
        .and_then(|_| output.sync_all())
        .map_err(|_| "output write")?;
    println!("dns provision ok prekeys={count}");
    Ok(())
}
fn parse_credentials(raw: &str) -> Result<(&str, &str, Option<&str>)> {
    let mut lines = raw.lines();
    let login = lines.next().ok_or("missing login")?;
    let password = lines.next().ok_or("missing password")?;
    let invite = lines.next();
    if lines.next().is_some() {
        return Err("invalid credential file");
    }
    let secret = invite
        .map(dmsg_protocol::auth::parse_invitation)
        .transpose()
        .map_err(|_| "invalid invitation")?;
    dmsg_protocol::auth::build_signup(login, password, secret.as_ref())
        .map_err(|_| "invalid credentials")?;
    Ok((login, password, invite))
}
fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("dns-provision") if args.len()==7 => provision(&args[1..]),
        Some("dns-send") if args.len()==4 => {
            let client=client(Path::new(&args[1]))?; let _guard=Guard(client.clone());
            let contact=peer_contact(&client,Path::new(&args[2]))?;
            client.send_dns(contact,text(Path::new(&args[3]),true)?,None).map_err(|_| "DNS send failed")?; println!("dns send ok"); Ok(())
        }
        Some("dns-fetch") if args.len()==2 => {
            let client=client(Path::new(&args[1]))?; let _guard=Guard(client.clone());
            let r=client.fetch_dns().map_err(|_| "DNS fetch failed")?;
            println!("dns fetch received={} unknown={} blocked={} undecryptable={} mismatch={}",r.received.len(),r.skipped_unknown,r.skipped_blocked,r.skipped_undecryptable,r.skipped_mismatch); Ok(())
        }
        Some("dns-negative-profiles") if args.len()==5 => {
            let code=text(Path::new(&args[1]),false)?;
            let p=dmsg_protocol::profile::parse(code.trim()).map_err(|_| "invalid profile")?;
            let wrong=fs::read(&args[2]).map_err(|_| "certificate fixture unavailable")?;
            for (path,cert,key) in [(&args[3],wrong.as_slice(),p.noise_pubkey),(&args[4],p.cert_der.as_slice(),[9;32])] {
                let code=dmsg_protocol::profile::build(&p.domain,cert,&key).map_err(|_| "invalid public fixture")?;
                let mut out=OpenOptions::new().write(true).create_new(true).mode(0o600).open(local(Path::new(path))?).map_err(|_| "fixture output unavailable")?;
                out.write_all(code.as_bytes()).and_then(|_| out.sync_all()).map_err(|_| "fixture write")?;
            } Ok(())
        }
        _=>Err("usage: dns-provision <db> <public-profile> <resolvers> <0600-credentials> <peer-contact> <contact-output> | dns-send <db> <peer-contact> <0600-message> | dns-fetch <db> | dns-negative-profiles <public-profile> <wrong-cert> <pin-output> <noise-output>"),
    }
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(category) = run(&args) {
        eprintln!("gate_peer: {category}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_file_is_bounded_and_password_whitespace_preserved() {
        let (login, password, invite) = parse_credentials("Alice\n  Pass word  \n").unwrap();
        assert_eq!(login, "Alice");
        assert_eq!(password, "  Pass word  ");
        assert_eq!(invite, None);
        assert!(parse_credentials("alice\nshort\n").is_err());
        assert!(parse_credentials("alice\npassword8\ndmsg://join/old\n").is_err());
        assert!(parse_credentials("alice\npassword8\nextra\nextra\n").is_err());
    }
}
