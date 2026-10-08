//! Owned, single-direction SRTP/SRTCP contexts using bundled Cisco libSRTP 2.7.0.
//! Fixed AES_CM_128_HMAC_SHA1_80, exact SSRC, no MKI, no system-library fallback.
//! libSRTP owns packet indices, ROC, SRTCP encryption indices and replay windows.

use std::{
    cell::Cell,
    ffi::{c_char, c_int, c_void, CStr},
    marker::PhantomData,
    ptr::NonNull,
    sync::OnceLock,
};

pub const MASTER_KEY_BYTES: usize = 16;
pub const MASTER_SALT_BYTES: usize = 14;
/// Master key followed by master salt, not a session key or an MKI.
pub const KEY_MATERIAL_BYTES: usize = MASTER_KEY_BYTES + MASTER_SALT_BYTES;
pub const AUTH_TAG_BYTES: usize = 10;
pub const SRTP_TRAILER_BYTES: usize = AUTH_TAG_BYTES;
pub const SRTCP_TRAILER_BYTES: usize = 4 + AUTH_TAG_BYTES;
pub const MIN_RTP_BYTES: usize = 12;
pub const MIN_RTCP_BYTES: usize = 8;
pub const MAX_PLAINTEXT_BYTES: usize = 4096;
pub const MAX_SRTP_BYTES: usize = MAX_PLAINTEXT_BYTES + SRTP_TRAILER_BYTES;
pub const MAX_SRTCP_BYTES: usize = MAX_PLAINTEXT_BYTES + SRTCP_TRAILER_BYTES;

extern "C" {
    // The shim returns NULL on success or a static NUL-terminated error string.
    fn dmsg_srtp_init() -> *const c_char;
    fn dmsg_srtp_create(key_material: *const u8, ssrc: u32, out: *mut *mut c_void)
        -> *const c_char;
    fn dmsg_srtp_destroy(session: *mut c_void);
    fn dmsg_srtp_transform(
        session: *mut c_void,
        input: *const u8,
        input_len: usize,
        output: *mut u8,
        output_capacity: usize,
        output_len: *mut usize,
        protect: c_int,
        rtcp: c_int,
    ) -> *const c_char;
}

fn checked(error: *const c_char) -> Result<(), String> {
    if error.is_null() {
        Ok(())
    } else {
        // Only the private shim supplies these static string pointers.
        Err(unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned())
    }
}

static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

struct State {
    session: NonNull<c_void>,
    ssrc: u32,
    // Explicitly !Sync, including if the pointer representation ever changes.
    _exclusive: PhantomData<Cell<()>>,
}

// Internal AES/HMAC contexts and replay windows are per-session heap state,
// with no thread-local ownership. Initialization/handler installation happens
// once before publication; the global crypto registry is then only read, and
// never shut down or modified. All operations require exclusive mutable access.
unsafe impl Send for State {}

impl State {
    fn new(key_material: &[u8; KEY_MATERIAL_BYTES], ssrc: u32) -> Result<Self, String> {
        INITIALIZED
            .get_or_init(|| checked(unsafe { dmsg_srtp_init() }))
            .clone()?;
        let mut session = std::ptr::null_mut();
        checked(unsafe { dmsg_srtp_create(key_material.as_ptr(), ssrc, &mut session) })?;
        Ok(Self {
            session: NonNull::new(session).ok_or("SRTP allocation failed")?,
            ssrc,
            _exclusive: PhantomData,
        })
    }

    fn apply(&mut self, packet: &[u8], protect: bool, rtcp: bool) -> Result<Vec<u8>, String> {
        let trailer = if rtcp {
            SRTCP_TRAILER_BYTES
        } else {
            SRTP_TRAILER_BYTES
        };
        let plaintext_len = if protect {
            packet.len()
        } else {
            packet
                .len()
                .checked_sub(trailer)
                .ok_or("Invalid SRTP packet length")?
        };
        let minimum = if rtcp { MIN_RTCP_BYTES } else { MIN_RTP_BYTES };
        if !(minimum..=MAX_PLAINTEXT_BYTES).contains(&plaintext_len) {
            return Err("Invalid SRTP packet length".into());
        }
        if rtcp {
            validate_rtcp_header(&packet[..plaintext_len], self.ssrc)?;
            if protect {
                validate_rtcp_compound(packet, self.ssrc)?;
            }
        } else {
            validate_rtp(&packet[..plaintext_len], self.ssrc, protect)?;
        }
        let expected_len = plaintext_len + if protect { trailer } else { 0 };
        let mut output = vec![0; expected_len];
        let mut actual_len = 0;
        checked(unsafe {
            dmsg_srtp_transform(
                self.session.as_ptr(),
                packet.as_ptr(),
                packet.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut actual_len,
                protect.into(),
                rtcp.into(),
            )
        })?;
        if actual_len != expected_len {
            return Err("Unexpected SRTP output length".into());
        }
        if !protect {
            if rtcp {
                validate_rtcp_compound(&output, self.ssrc)?;
            } else {
                validate_rtp(&output, self.ssrc, true)?;
            }
        }
        Ok(output)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe { dmsg_srtp_destroy(self.session.as_ptr()) }
    }
}

/// Outbound media and whole-compound feedback for one directional key and SSRC.
/// Neither cloneable nor Sync; may be moved to another thread.
///
/// Each new generation MUST supply fresh key/salt material. Recreating a
/// context with an old key resets counters/replay state and is not a restart.
/// The caller owns and must erase its input key buffer; the shim erases its
/// temporary copy immediately, and libSRTP erases derived key state on drop.
pub struct Sender(State);

impl Sender {
    pub fn new(key_material: &[u8; KEY_MATERIAL_BYTES], ssrc: u32) -> Result<Self, String> {
        State::new(key_material, ssrc).map(Self)
    }

    /// Protect one complete RTP packet (including its clear RTP header).
    /// Duplicate outbound indices are rejected, not silently re-encrypted.
    pub fn protect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>, String> {
        self.0.apply(packet, true, false)
    }

    /// Protect the ENTIRE RTCP compound once, not each SR/RR/SDES/APP separately.
    /// The first report must be SR or RR from the configured SSRC.
    pub fn protect_rtcp(&mut self, compound: &[u8]) -> Result<Vec<u8>, String> {
        self.0.apply(compound, true, true)
    }
}

/// Inbound media and whole-compound feedback for one directional key and SSRC.
/// Authentication and replay failures return errors, never plaintext.
/// Fresh-generation and key-buffer ownership requirements match [`Sender`].
pub struct Receiver(State);

impl Receiver {
    pub fn new(key_material: &[u8; KEY_MATERIAL_BYTES], ssrc: u32) -> Result<Self, String> {
        State::new(key_material, ssrc).map(Self)
    }

    pub fn unprotect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>, String> {
        self.0.apply(packet, false, false)
    }

    pub fn unprotect_rtcp(&mut self, compound: &[u8]) -> Result<Vec<u8>, String> {
        self.0.apply(compound, false, true)
    }
}

fn u16_at(packet: &[u8], offset: usize) -> usize {
    u16::from_be_bytes([packet[offset], packet[offset + 1]]) as usize
}

fn expected_ssrc(packet: &[u8], offset: usize, ssrc: u32) -> Result<(), String> {
    if packet[offset..offset + 4] != ssrc.to_be_bytes() {
        return Err("SRTP unexpected SSRC".into());
    }
    Ok(())
}

fn validate_rtp(packet: &[u8], ssrc: u32, plaintext: bool) -> Result<(), String> {
    if packet.len() < MIN_RTP_BYTES || packet[0] >> 6 != 2 {
        return Err("Invalid RTP header".into());
    }
    expected_ssrc(packet, 8, ssrc)?;
    let mut header_len = MIN_RTP_BYTES + usize::from(packet[0] & 0x0f) * 4;
    if header_len > packet.len() {
        return Err("Invalid RTP CSRC length".into());
    }
    if packet[0] & 0x10 != 0 {
        if packet.len() - header_len < 4 {
            return Err("Invalid RTP extension length".into());
        }
        header_len += 4 + u16_at(packet, header_len + 2) * 4;
        if header_len > packet.len() {
            return Err("Invalid RTP extension length".into());
        }
    }
    // Padding octets are encrypted: inspect only plaintext, after auth on RX.
    if plaintext && packet[0] & 0x20 != 0 {
        let padding = usize::from(packet[packet.len() - 1]);
        if padding == 0 || padding > packet.len() - header_len {
            return Err("Invalid RTP padding".into());
        }
    }
    Ok(())
}

fn report_minimum(header: &[u8]) -> Result<usize, String> {
    let reports = usize::from(header[0] & 0x1f);
    match header[1] {
        200 => Ok(28 + reports * 24),
        201 => Ok(8 + reports * 24),
        _ => Err("RTCP compound must start with SR or RR".into()),
    }
}

fn validate_rtcp_header(packet: &[u8], ssrc: u32) -> Result<(), String> {
    if packet.len() < MIN_RTCP_BYTES || !packet.len().is_multiple_of(4) || packet[0] >> 6 != 2 {
        return Err("Invalid RTCP header".into());
    }
    expected_ssrc(packet, 4, ssrc)?;
    let first_len = (u16_at(packet, 2) + 1) * 4;
    if first_len < report_minimum(packet)? || first_len > packet.len() {
        return Err("Invalid RTCP report length".into());
    }
    Ok(())
}

// Structural boundary only: the caller validates its SDES/APP feedback profile.
// No encrypted subpacket lengths/content are parsed before authentication.
fn validate_rtcp_compound(packet: &[u8], ssrc: u32) -> Result<(), String> {
    validate_rtcp_header(packet, ssrc)?;
    let mut offset = 0;
    while offset < packet.len() {
        let block = &packet[offset..];
        if block.len() < 4 || block[0] >> 6 != 2 || !(192..=223).contains(&block[1]) {
            return Err("Invalid RTCP compound header".into());
        }
        let block_len = (u16_at(block, 2) + 1) * 4;
        if block_len > block.len() {
            return Err("Invalid RTCP compound length".into());
        }
        let count = usize::from(block[0] & 0x1f);
        let minimum = match block[1] {
            200 | 201 => report_minimum(block)?,
            202 | 203 => 4 + count * 4,
            204 => 12,
            _ => 4,
        };
        let padding = if block[0] & 0x20 != 0 {
            let padding = usize::from(block[block_len - 1]);
            if block_len != block.len() || padding == 0 || !padding.is_multiple_of(4) {
                return Err("Invalid RTCP padding".into());
            }
            padding
        } else {
            0
        };
        if block_len < minimum || padding > block_len - minimum {
            return Err("Invalid RTCP compound length".into());
        }
        offset += block_len;
    }
    Ok(())
}
