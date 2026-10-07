//! Small ownership boundary around the bundled, statically linked Opus release.
//! No system-library fallback, model loading, PLC or FEC entry point is exposed.

use std::{ffi::c_int, ptr::NonNull};

pub const FRAME_SAMPLES: usize = 320;
pub const MAX_PACKET_BYTES: usize = 4000;
const SAMPLE_RATE: c_int = 16_000;

#[repr(C)]
struct OpusEncoder {
    _opaque: [u8; 0],
}
#[repr(C)]
struct OpusDecoder {
    _opaque: [u8; 0],
}

extern "C" {
    fn opus_encoder_create(
        fs: c_int,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoder;
    fn opus_encoder_destroy(st: *mut OpusEncoder);
    fn opus_encoder_get_size(channels: c_int) -> c_int;
    fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;
    fn opus_encode(
        st: *mut OpusEncoder,
        pcm: *const i16,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: c_int,
    ) -> c_int;
    fn opus_decoder_create(fs: c_int, channels: c_int, error: *mut c_int) -> *mut OpusDecoder;
    fn opus_decoder_destroy(st: *mut OpusDecoder);
    fn opus_decoder_ctl(st: *mut OpusDecoder, request: c_int, ...) -> c_int;
    fn opus_decode(
        st: *mut OpusDecoder,
        data: *const u8,
        len: c_int,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_packet_get_nb_samples(packet: *const u8, len: c_int, fs: c_int) -> c_int;
    fn opus_packet_get_nb_channels(packet: *const u8) -> c_int;
    fn opus_packet_get_bandwidth(packet: *const u8) -> c_int;
    fn opus_packet_parse(
        data: *const u8,
        len: c_int,
        toc: *mut u8,
        frames: *mut *const u8,
        size: *mut i16,
        payload_offset: *mut c_int,
    ) -> c_int;
}

fn checked(code: c_int) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err("Opus configuration failed".into())
    }
}

/// One continuous fixed 10 kbit/s, mono, 16 kHz PCM16 VOIP encoder.
pub struct Encoder(NonNull<OpusEncoder>);
// State has no thread-local data; exclusive mutable access serializes calls.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> Result<Self, String> {
        let mut error = 0;
        let ptr = unsafe { opus_encoder_create(SAMPLE_RATE, 1, 2048, &mut error) };
        let encoder = Self(NonNull::new(ptr).ok_or("Opus encoder allocation failed")?);
        checked(error)?;
        for (request, value) in [
            (4002, 10_000), // bitrate
            (4006, 1),      // VBR
            (4020, 0),      // unconstrained VBR
            (4024, 3001),   // VOICE
            (4004, 1103),   // maximum wideband
            (4008, -1000),  // automatic bandwidth
            (4010, 10),     // encoder complexity
            (4036, 16),     // PCM significant bits
            (4012, 0),      // no FEC
            (4014, 0),      // no expected loss
            (4016, 0),      // no DTX
        ] {
            checked(unsafe { opus_encoder_ctl(encoder.0.as_ptr(), request, value as c_int) })?;
        }
        Ok(encoder)
    }

    pub fn lookahead(&self) -> Result<u32, String> {
        let mut samples: c_int = 0;
        checked(unsafe { opus_encoder_ctl(self.0.as_ptr(), 4027, &mut samples as *mut c_int) })?;
        u32::try_from(samples).map_err(|_| "Invalid Opus lookahead".into())
    }

    /// Opus documents its state as contiguous and copyable. Copying for a
    /// snapshot allows flushing a preview without changing the live encoder.
    pub fn copy(&self) -> Result<Self, String> {
        let copy = Self::new()?;
        let size = unsafe { opus_encoder_get_size(1) };
        if size <= 0 {
            return Err("Invalid Opus encoder size".into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.0.as_ptr().cast::<u8>(),
                copy.0.as_ptr().cast::<u8>(),
                size as usize,
            );
        }
        Ok(copy)
    }

    pub fn encode(&mut self, pcm: &[i16; FRAME_SAMPLES]) -> Result<Vec<u8>, String> {
        let mut packet = vec![0u8; MAX_PACKET_BYTES];
        let size = unsafe {
            opus_encode(
                self.0.as_ptr(),
                pcm.as_ptr(),
                FRAME_SAMPLES as c_int,
                packet.as_mut_ptr(),
                MAX_PACKET_BYTES as c_int,
            )
        };
        if size <= 0 {
            return Err("Opus encoding failed".into());
        }
        packet.truncate(size as usize);
        validate_packet(&packet)?;
        Ok(packet)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.0.as_ptr()) }
    }
}

/// Structural validation rejects empty frames (which Opus would conceal),
/// stereo, non-20ms packets and bandwidth beyond this profile's wideband cap.
pub fn validate_packet(packet: &[u8]) -> Result<(), String> {
    if packet.is_empty() || packet.len() > MAX_PACKET_BYTES {
        return Err("Invalid Opus packet length".into());
    }
    let mut toc = 0;
    let mut frames = [std::ptr::null(); 48];
    let mut sizes = [0i16; 48];
    let mut offset = 0;
    let count = unsafe {
        opus_packet_parse(
            packet.as_ptr(),
            packet.len() as c_int,
            &mut toc,
            frames.as_mut_ptr(),
            sizes.as_mut_ptr(),
            &mut offset,
        )
    };
    if count != 1
        || sizes[0] <= 0
        || unsafe { opus_packet_get_nb_channels(packet.as_ptr()) } != 1
        || unsafe {
            opus_packet_get_nb_samples(packet.as_ptr(), packet.len() as c_int, SAMPLE_RATE)
        } != FRAME_SAMPLES as c_int
        || unsafe { opus_packet_get_bandwidth(packet.as_ptr()) } > 1103
    {
        return Err("Invalid Opus voice packet".into());
    }
    Ok(())
}

pub struct Decoder(NonNull<OpusDecoder>);
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self, String> {
        Self::with_complexity(7)
    }

    /// Complexity 0 is useful for the bundled OSCE activation gate. Production
    /// callers use `new`, which explicitly sets and reads back complexity 7.
    pub fn with_complexity(complexity: c_int) -> Result<Self, String> {
        if complexity != 0 && complexity != 7 {
            return Err("Invalid decoder complexity".into());
        }
        let mut error = 0;
        let ptr = unsafe { opus_decoder_create(SAMPLE_RATE, 1, &mut error) };
        let decoder = Self(NonNull::new(ptr).ok_or("Opus decoder allocation failed")?);
        checked(error)?;
        checked(unsafe { opus_decoder_ctl(decoder.0.as_ptr(), 4010, complexity) })?;
        let mut actual: c_int = -1;
        checked(unsafe { opus_decoder_ctl(decoder.0.as_ptr(), 4011, &mut actual as *mut c_int) })?;
        if actual != complexity {
            return Err("Opus decoder complexity unavailable".into());
        }
        Ok(decoder)
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<[i16; FRAME_SAMPLES], String> {
        validate_packet(packet)?;
        let mut pcm = [0; FRAME_SAMPLES];
        let samples = unsafe {
            opus_decode(
                self.0.as_ptr(),
                packet.as_ptr(),
                packet.len() as c_int,
                pcm.as_mut_ptr(),
                FRAME_SAMPLES as c_int,
                0,
            )
        };
        if samples != FRAME_SAMPLES as c_int {
            return Err("Opus decoding failed".into());
        }
        Ok(pcm)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.0.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_profile_controls_are_applied() {
        let encoder = Encoder::new().unwrap();
        for (request, expected) in [
            (4001, 2048),
            (4003, 10_000),
            (4005, 1103),
            (4007, 1),
            (4011, 10),
            (4013, 0),
            (4015, 0),
            (4017, 0),
            (4021, 0),
            (4025, 3001),
            (4037, 16),
        ] {
            let mut actual: c_int = -9999;
            checked(unsafe {
                opus_encoder_ctl(encoder.0.as_ptr(), request, &mut actual as *mut c_int)
            })
            .unwrap();
            assert_eq!(actual, expected);
        }
    }
}
