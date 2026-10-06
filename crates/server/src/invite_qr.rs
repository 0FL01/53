//! Interactive administrator-only QR view. No URI or plaintext token output.
use qrcode::{Color, EcLevel, QrCode};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Args {
    Issue(Option<String>),
    Show(String),
}

pub fn args(rest: &[String]) -> Option<Args> {
    match rest {
        [] => Some(Args::Issue(None)),
        [flag, ttl] if flag == "--ttl" => ttl
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .map(|n| Args::Issue(Some(n.to_string()))),
        [flag, file] if flag == "--file" && !file.is_empty() => Some(Args::Show(file.clone())),
        _ => None,
    }
}

/// Reserve a private directory on the existing data volume, not a new store.
pub fn destination(data: &Path) -> Result<PathBuf, &'static str> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let dir = data.join("invites");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err("private-directory-unavailable"),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| "private-directory-unavailable")?;
    let owner = std::fs::metadata(data)
        .map_err(|_| "private-directory-unavailable")?
        .uid();
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 || meta.uid() != owner {
        return Err("invalid-private-directory");
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "random-unavailable")?;
    let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Ok(dir.join(format!("{name}.invite")))
}

/// Fixed truecolor contrast, four-module quiet zone, two square modules per cell.
/// ANSI reset after each line keeps the user's terminal state intact.
pub fn render(token: &str) -> Result<String, &'static str> {
    dmsg_protocol::auth::parse_invitation(token).map_err(|_| "invalid-invitation")?;
    let code = QrCode::with_error_correction_level(token.as_bytes(), EcLevel::M)
        .map_err(|_| "qr-unavailable")?;
    let size = code.width() + 8;
    let dark = |x: usize, y: usize| {
        x >= 4 && y >= 4 && x < size - 4 && y < size - 4 && code[(x - 4, y - 4)] == Color::Dark
    };
    let mut out = String::new();
    for y in (0..size).step_by(2) {
        out.push_str("\x1b[38;2;0;0;0m\x1b[48;2;255;255;255m");
        for x in 0..size {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => ' ',
                (true, false) => '▀',
                (false, true) => '▄',
                (true, true) => '█',
            });
        }
        out.push_str("\x1b[0m\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_options_and_terminal_roundtrip() {
        let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(args(&[]), Some(Args::Issue(None)));
        assert_eq!(
            args(&strings(&["--ttl", "3600"])),
            Some(Args::Issue(Some("3600".into())))
        );
        assert_eq!(
            args(&strings(&["--file", "/private/invite"])),
            Some(Args::Show("/private/invite".into()))
        );
        for invalid in [
            &["--ttl", "0"][..],
            &["--ttl", "-1"],
            &["--ttl", "9223372036854775808"],
            &["--file", ""],
            &["--file", "x", "--ttl", "1"],
        ] {
            assert!(args(&strings(invalid)).is_none());
        }
        // All-zero fixture is NOT a production secret. Decode the terminal glyphs,
        // not an encoder-internal matrix, so glyph order and quiet zone are tested.
        let token = dmsg_protocol::auth::build_invitation(&[0u8; 32]);
        let rendered = render(&token).unwrap();
        assert!(!rendered.contains(&token));
        let rows: Vec<Vec<char>> = rendered
            .lines()
            .map(|line| {
                line.strip_prefix("\x1b[38;2;0;0;0m\x1b[48;2;255;255;255m")
                    .unwrap()
                    .strip_suffix("\x1b[0m")
                    .unwrap()
                    .chars()
                    .collect()
            })
            .collect();
        let width = rows[0].len();
        let scale = 8;
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(
            width * scale,
            rows.len() * 2 * scale,
            |x, y| {
                let c = rows[y / scale / 2][x / scale];
                let dark = if y / scale % 2 == 0 {
                    c == '▀' || c == '█'
                } else {
                    c == '▄' || c == '█'
                };
                if dark {
                    0
                } else {
                    255
                }
            },
        );
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].decode().unwrap().1, token);
        assert!(render("dmsg://invite/not-allowed").is_err());
    }
}
