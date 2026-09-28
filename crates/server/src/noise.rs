//! Noise IK P2: паттерн, keygen, загрузка static-ключа сервера.
//! Ключ — сырые 32 байта ТОЛЬКО из файла (никогда env/argv). Паттерн один на всех:
//! будущее ядро использует тот же snow и тот же паттерн, иначе рассинхрон handshake.

use std::path::Path;

/// Noise-паттерн msgd v1: 1-RTT, static сервера pre-known инициатору из invite.
pub const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

/// Длина сырового X25519 static-ключа.
pub const KEY_LEN: usize = 32;

/// Сгенерировать keypair для PATTERN. Возвращает (private, public).
pub fn generate() -> Result<([u8; KEY_LEN], [u8; KEY_LEN]), snow::Error> {
    let params: snow::params::NoiseParams = PATTERN.parse()?;
    let builder = snow::Builder::new(params);
    let kp = builder.generate_keypair()?;
    let mut privk = [0u8; KEY_LEN];
    let mut pubk = [0u8; KEY_LEN];
    privk.copy_from_slice(&kp.private[..KEY_LEN]);
    pubk.copy_from_slice(&kp.public[..KEY_LEN]);
    Ok((privk, pubk))
}

/// Загрузить private static-ключ из файла: ровно 32 байта, иначе fail-closed.
pub fn load_private<P: AsRef<Path>>(path: P) -> Result<[u8; KEY_LEN], String> {
    let raw = std::fs::read(path.as_ref())
        .map_err(|e| format!("noise key {}: {e}", path.as_ref().display()))?;
    if raw.len() != KEY_LEN {
        return Err(format!("noise key: want {KEY_LEN} bytes, got {}", raw.len()));
    }
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&raw);
    Ok(k)
}

/// Public-ключ для private (сырые 32 байта) — сборка bootstrap, сверки.
pub fn pubkey_of(privk: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let secret = x25519_dalek::StaticSecret::from(*privk);
    *x25519_dalek::PublicKey::from(&secret).as_bytes()
}

/// Вывести public-ключ для private из файла (hex) — нужен bootstrap в P3.
/// Деривация стандартным X25519 basepoint mult (тот же, что внутри snow).
pub fn pubkey_hex<P: AsRef<Path>>(path: P) -> Result<String, String> {
    let privk = load_private(path)?;
    Ok(hex_of(&pubkey_of(&privk)))
}

fn hex_of(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

/// keygen: создать файл с 32 случайными байтами (0600), отказ при существующем.
/// Печатает public hex в stdout (для bootstrap P3 и сверки).
pub fn keygen<P: AsRef<Path>>(path: P) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    if path.as_ref().exists() {
        return Err(format!("refuse: {} exists", path.as_ref().display()));
    }
    let (privk, pubk) = generate().map_err(|e| format!("generate: {e}"))?;
    let mut opt = std::fs::OpenOptions::new();
    opt.write(true).create_new(true).mode(0o600);
    use std::io::Write;
    opt.open(path.as_ref())
        .and_then(|mut f| f.write_all(&privk))
        .map_err(|e| format!("write {}: {e}", path.as_ref().display()))?;
    println!("{}", hex_of(&pubk));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_parses_and_pair_agrees() {
        let dir = std::env::temp_dir().join(format!("msgd-kg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("k");
        // keygen через файл
        keygen(&f).unwrap();
        assert!(keygen(&f).is_err()); // refuse-if-exists
        let loaded = load_private(&f).unwrap();
        assert_eq!(loaded.len(), KEY_LEN);
        // snow-пара из generate() согласуется с x25519-деривацией
        let (privk, pubk) = generate().unwrap();
        let secret = x25519_dalek::StaticSecret::from(privk);
        assert_eq!(x25519_dalek::PublicKey::from(&secret).as_bytes(), &pubk);
        // pubkey_hex файла совпадает с деривацией
        let dir2 = std::env::temp_dir().join(format!("msgd-kp-{}", std::process::id()));
        std::fs::create_dir_all(&dir2).unwrap();
        let f2 = dir2.join("k");
        std::fs::write(&f2, privk).unwrap();
        let h = pubkey_hex(&f2).unwrap();
        assert_eq!(h, hex_of(&pubk));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn bad_length_rejected() {
        let dir = std::env::temp_dir().join(format!("msgd-kb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("k");
        std::fs::write(&f, [1u8; 7]).unwrap();
        assert!(load_private(&f).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
