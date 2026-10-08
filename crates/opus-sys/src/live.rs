//! Memory-only live codec for M1-V. This does not change the DVN/note profile.
//! One immutable profile per run: 12 kbit/s, mono 16 kHz, 20/40/60 ms screening.
//! Packet caps are passed to Opus, not applied by truncating encoded bytes.

use super::*;

pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const MAX_FRAME_SAMPLES: usize = 960;
pub const MAX_PACKET_BYTES: usize = 150;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiveProfile {
    Ms20,
    #[default]
    Ms40,
    Ms60,
}

impl LiveProfile {
    pub const fn duration_ms(self) -> u32 {
        match self {
            Self::Ms20 => 20,
            Self::Ms40 => 40,
            Self::Ms60 => 60,
        }
    }

    pub const fn samples(self) -> usize {
        self.duration_ms() as usize * 16
    }

    pub const fn packet_cap(self) -> usize {
        // The same 20 kbit/s payload ceiling for each diagnostic duration.
        self.duration_ms() as usize * 5 / 2
    }

    pub const fn rtp_ticks(self) -> u32 {
        // Opus RTP clock is 48 kHz even though the PCM clock is 16 kHz.
        self.duration_ms() * 48
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketInfo {
    pub frame_count: u8,
    pub bandwidth: u16,
    /// Coded frames of size <=1 invoke Opus PLC/DTX, not fresh decoded speech.
    /// A present packet can therefore be structurally valid yet conceal audio.
    pub concealed_samples: u16,
}

/// Validate the complete Opus packet, including legal TOC-only DTX forms.
/// Authentication, freshness and playout deadlines belong to the media owner.
pub fn validate_packet(profile: LiveProfile, packet: &[u8]) -> Result<PacketInfo, String> {
    if packet.is_empty() || packet.len() > profile.packet_cap() {
        return Err("Invalid live Opus packet length".into());
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
    if !(1..=48).contains(&count)
        || unsafe { opus_packet_get_nb_channels(packet.as_ptr()) } != 1
        || unsafe {
            opus_packet_get_nb_samples(packet.as_ptr(), packet.len() as c_int, SAMPLE_RATE)
        } != profile.samples() as c_int
    {
        return Err("Invalid live Opus packet".into());
    }
    let bandwidth = unsafe { opus_packet_get_bandwidth(packet.as_ptr()) };
    if !(1101..=1103).contains(&bandwidth) {
        return Err("Invalid live Opus bandwidth".into());
    }
    let empty = sizes[..count as usize]
        .iter()
        .filter(|size| **size <= 1)
        .count();
    Ok(PacketInfo {
        frame_count: count as u8,
        bandwidth: bandwidth as u16,
        concealed_samples: (empty * profile.samples() / count as usize) as u16,
    })
}

/// Do not log packet bytes: these are plaintext encoded audio, not ciphertext.
pub struct EncodedPacket {
    pub bytes: Vec<u8>,
    pub info: PacketInfo,
    /// Encoder state, not an authoritative source-activity/quality denominator.
    pub in_dtx: bool,
}

pub struct LiveEncoder {
    state: NonNull<OpusEncoder>,
    profile: LiveProfile,
}

// Opus owns no thread-local state; all codec access is exclusive mutable access.
unsafe impl Send for LiveEncoder {}

impl LiveEncoder {
    pub fn new(profile: LiveProfile) -> Result<Self, String> {
        let mut error = 0;
        let state = unsafe { opus_encoder_create(SAMPLE_RATE, 1, 2048, &mut error) };
        let encoder = Self {
            state: NonNull::new(state).ok_or("Live Opus encoder allocation failed")?,
            profile,
        };
        checked(error)?;
        for (request, value) in [
            (4002, 12_000), // bitrate, in addition to the encoding-time cap
            (4006, 1),      // VBR; speech does not obey the CVBR constraint
            (4020, 0),      // do not pretend CVBR bounds speech peaks
            (4024, 3001),   // VOICE
            (4004, 1103),   // maximum WB, not forced WB
            (4008, -1000),  // automatic bandwidth
            (4010, 10),
            (4036, 16),
            (4012, 0), // no FEC
            (4014, 0), // no expected loss
            (4016, 1), // DTX; first probe transmits all returned packets
        ] {
            checked(unsafe { opus_encoder_ctl(encoder.state.as_ptr(), request, value as c_int) })?;
        }
        Ok(encoder)
    }

    pub fn lookahead(&self) -> Result<u32, String> {
        let mut samples: c_int = 0;
        checked(unsafe {
            opus_encoder_ctl(self.state.as_ptr(), 4027, &mut samples as *mut c_int)
        })?;
        u32::try_from(samples).map_err(|_| "Invalid live Opus lookahead".into())
    }

    /// Exactly one interval. Never retry the same PCM after an encoding error:
    /// the state may already have advanced. Retire the run instead.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedPacket, String> {
        if pcm.len() != self.profile.samples() {
            return Err("Invalid live Opus PCM interval".into());
        }
        let mut packet = vec![0; self.profile.packet_cap()];
        let size = unsafe {
            opus_encode(
                self.state.as_ptr(),
                pcm.as_ptr(),
                pcm.len() as c_int,
                packet.as_mut_ptr(),
                packet.len() as c_int,
            )
        };
        if size <= 0 || size as usize > packet.len() {
            return Err("Live Opus encoding failed".into());
        }
        packet.truncate(size as usize);
        let info = validate_packet(self.profile, &packet)?;
        let mut in_dtx: c_int = 0;
        checked(unsafe { opus_encoder_ctl(self.state.as_ptr(), 4049, &mut in_dtx as *mut c_int) })?;
        Ok(EncodedPacket {
            bytes: packet,
            info,
            in_dtx: in_dtx != 0,
        })
    }
}

impl Drop for LiveEncoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.state.as_ptr()) }
    }
}

pub struct LiveDecoder {
    state: NonNull<OpusDecoder>,
    profile: LiveProfile,
}

unsafe impl Send for LiveDecoder {}

impl LiveDecoder {
    pub fn new(profile: LiveProfile) -> Result<Self, String> {
        Self::with_complexity(profile, 7)
    }

    /// The additional values are only for activation comparisons: 0/7 on
    /// lossless packets for NoLACE, 4/5 on erasures for isolated Deep PLC.
    pub fn with_complexity(profile: LiveProfile, complexity: u8) -> Result<Self, String> {
        if !matches!(complexity, 0 | 4 | 5 | 7) {
            return Err("Invalid live Opus decoder complexity".into());
        }
        let mut error = 0;
        let state = unsafe { opus_decoder_create(SAMPLE_RATE, 1, &mut error) };
        let decoder = Self {
            state: NonNull::new(state).ok_or("Live Opus decoder allocation failed")?,
            profile,
        };
        checked(error)?;
        checked(unsafe { opus_decoder_ctl(decoder.state.as_ptr(), 4010, complexity as c_int) })?;
        let mut actual: c_int = -1;
        checked(unsafe {
            opus_decoder_ctl(decoder.state.as_ptr(), 4011, &mut actual as *mut c_int)
        })?;
        if actual != complexity as c_int {
            return Err("Live Opus decoder complexity unavailable".into());
        }
        Ok(decoder)
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, String> {
        validate_packet(self.profile, packet)?;
        self.decode_inner(packet.as_ptr(), packet.len() as c_int)
    }

    /// One missing interval, exactly once on its playout deadline. Invalid or
    /// unauthenticated incoming bytes must not be passed as an implicit erasure.
    pub fn conceal(&mut self) -> Result<Vec<i16>, String> {
        self.decode_inner(std::ptr::null(), 0)
    }

    fn decode_inner(&mut self, data: *const u8, len: c_int) -> Result<Vec<i16>, String> {
        let mut pcm = vec![0; self.profile.samples()];
        let samples = unsafe {
            opus_decode(
                self.state.as_ptr(),
                data,
                len,
                pcm.as_mut_ptr(),
                pcm.len() as c_int,
                0,
            )
        };
        if samples != self.profile.samples() as c_int {
            return Err("Live Opus decoding failed".into());
        }
        Ok(pcm)
    }
}

impl Drop for LiveDecoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.state.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(count: usize) -> Vec<i16> {
        (0..count)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE_HZ as f64;
                let f = 120.0 + 15.0 * (t * 2.1).sin();
                let envelope = 0.6 + 0.35 * (t * 7.0).sin();
                let value = (1..=12)
                    .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64)
                    .sum::<f64>();
                (value * envelope * 9000.0) as i16
            })
            .collect()
    }

    #[test]
    fn controls_duration_caps_and_rtp_clock_are_explicit() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            let encoder = LiveEncoder::new(profile).unwrap();
            for (request, expected) in [
                (4001, 2048),
                (4003, 12_000),
                (4005, 1103),
                (4007, 1),
                (4011, 10),
                (4013, 0),
                (4015, 0),
                (4017, 1),
                (4021, 0),
                (4025, 3001),
                (4037, 16),
            ] {
                let mut actual: c_int = -9999;
                checked(unsafe {
                    opus_encoder_ctl(encoder.state.as_ptr(), request, &mut actual as *mut c_int)
                })
                .unwrap();
                assert_eq!(actual, expected);
            }
            assert_eq!(profile.rtp_ticks(), (profile.samples() * 3) as u32);
            assert_eq!(profile.packet_cap(), profile.duration_ms() as usize * 5 / 2);
            assert_eq!(encoder.lookahead().unwrap(), 104);
        }
    }

    #[test]
    fn capped_speech_noise_and_dtx_roundtrip_without_clipping_packet_bytes() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            let mut encoder = LiveEncoder::new(profile).unwrap();
            let mut decoder = LiveDecoder::new(profile).unwrap();
            let mut noise_state = 1u32;
            let voice = speech(profile.samples() * 30);
            for pcm in voice.chunks(profile.samples()) {
                let encoded = encoder.encode(pcm).unwrap();
                assert!(encoded.bytes.len() <= profile.packet_cap());
                assert_eq!(
                    decoder.decode(&encoded.bytes).unwrap().len(),
                    profile.samples()
                );
            }
            for _ in 0..20 {
                let noise: Vec<i16> = (0..profile.samples())
                    .map(|_| {
                        noise_state = noise_state.wrapping_mul(1664525).wrapping_add(1013904223);
                        (noise_state >> 16) as i16
                    })
                    .collect();
                let encoded = encoder.encode(&noise).unwrap();
                assert!(encoded.bytes.len() <= profile.packet_cap());
                assert_eq!(
                    decoder.decode(&encoded.bytes).unwrap().len(),
                    profile.samples()
                );
            }
            let mut tiny_dtx = 0;
            for _ in 0..150 {
                let encoded = encoder.encode(&vec![0; profile.samples()]).unwrap();
                if encoded.in_dtx && encoded.bytes.len() <= 2 {
                    tiny_dtx += 1;
                }
                assert_eq!(
                    decoder.decode(&encoded.bytes).unwrap().len(),
                    profile.samples()
                );
            }
            assert!(tiny_dtx > 10, "fixture must actually enter DTX");
        }
    }

    #[test]
    fn live_validator_allows_dtx_but_does_not_weaken_note_validator() {
        let tiny = [0x50]; // one empty SILK WB40 packet, valid live DTX/PLC form
        let info = validate_packet(LiveProfile::Ms40, &tiny).unwrap();
        assert_eq!(info.concealed_samples, 640);
        assert!(super::super::validate_packet(&tiny).is_err());
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            assert!(validate_packet(profile, &[]).is_err());
            assert!(validate_packet(profile, &vec![0; profile.packet_cap() + 1]).is_err());
            assert!(validate_packet(profile, &[0xff]).is_err());
        }
        assert!(validate_packet(LiveProfile::Ms20, &tiny).is_err());
        assert!(validate_packet(LiveProfile::Ms60, &tiny).is_err());
        assert!(validate_packet(LiveProfile::Ms40, &[0x54]).is_err()); // stereo
        assert!(validate_packet(LiveProfile::Ms40, &[0xe8]).is_err()); // fullband
        let mut encoder = LiveEncoder::new(LiveProfile::Ms40).unwrap();
        assert!(encoder.encode(&[0; 320]).is_err());
        assert!(encoder.encode(&[]).is_err());
    }

    #[test]
    fn exact_cold_plc_consecutive_erasures_and_speech_recovery() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            let mut encoder = LiveEncoder::new(profile).unwrap();
            let mut decoder = LiveDecoder::new(profile).unwrap();
            assert_eq!(decoder.conceal().unwrap().len(), profile.samples());
            for pcm in speech(profile.samples() * 50).chunks(profile.samples()) {
                decoder.decode(&encoder.encode(pcm).unwrap().bytes).unwrap();
            }
            for _ in 0..5 {
                assert_eq!(decoder.conceal().unwrap().len(), profile.samples());
            }
            for pcm in speech(profile.samples() * 20).chunks(profile.samples()) {
                assert_eq!(
                    decoder
                        .decode(&encoder.encode(pcm).unwrap().bytes)
                        .unwrap()
                        .len(),
                    profile.samples()
                );
            }
        }
    }

    #[test]
    fn nolace_40ms_and_isolated_deep_plc_change_actual_pcm() {
        let profile = LiveProfile::Ms40;
        let mut encoder = LiveEncoder::new(profile).unwrap();
        let packets: Vec<_> = speech(5 * SAMPLE_RATE_HZ as usize)
            .chunks(profile.samples())
            .map(|pcm| encoder.encode(pcm).unwrap().bytes)
            .collect();
        let mut plain = LiveDecoder::with_complexity(profile, 0).unwrap();
        let mut enhanced = LiveDecoder::new(profile).unwrap();
        let mut wb_packets = 0;
        let mut differing = 0;
        for (index, packet) in packets.iter().enumerate() {
            if packet[0] >> 3 == 10 {
                wb_packets += 1;
            } // SILK WB40
            let base = plain.decode(packet).unwrap();
            let nolace = enhanced.decode(packet).unwrap();
            if index >= 25 {
                differing += base.iter().zip(nolace).filter(|(a, b)| **a != *b).count();
            }
        }
        assert!(wb_packets > 50, "fixture must exercise supported SILK WB40");
        assert!(
            differing > 1000,
            "NoLACE actual PCM must differ after warmup"
        );

        let mut base = LiveDecoder::with_complexity(profile, 4).unwrap();
        let mut deep = LiveDecoder::with_complexity(profile, 5).unwrap();
        for packet in &packets {
            base.decode(packet).unwrap();
            deep.decode(packet).unwrap();
        }
        let mut plc_differing = 0;
        for _ in 0..4 {
            let a = base.conceal().unwrap();
            let b = deep.conceal().unwrap();
            plc_differing += a.iter().zip(b).filter(|(a, b)| **a != *b).count();
        }
        assert!(
            plc_differing > 100,
            "Deep PLC must affect actual erased PCM"
        );
        assert!(LiveDecoder::with_complexity(profile, 6).is_err());
    }
}
