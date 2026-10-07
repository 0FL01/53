//! Memory-only fixed-profile voice notes, with continuous Opus state.
//!
//! Container v1/profile1: `DVN\0`, version:u8, profile:u8,
//! original sample_count:u32 LE, pre_skip:u32 LE, 64 waveform bytes, then
//! canonical ULEB128 length + Opus packet pairs. Packet count is derived from
//! ceil((sample_count + pre_skip) / 320), so missing/extra packets are errors.
//! The AEAD media layer authenticates payload bytes; this is not a checksum.

use dmsg_opus_sys::{Decoder, Encoder, FRAME_SAMPLES, MAX_PACKET_BYTES};
use std::ops::Range;

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_SAMPLES: u32 = 60 * SAMPLE_RATE;
/// Leaves room for the encrypted manifest and per-chunk AEAD under 128 KiB.
pub const MAX_CONTAINER_BYTES: usize = 120 * 1024;
pub const WAVEFORM_BYTES: usize = 64;
pub const MAX_BATCH_SAMPLES: usize = 1600;
const HEADER_BYTES: usize = 14 + WAVEFORM_BYTES;
const MAGIC: &[u8; 4] = b"DVN\0";
const VERSION: u8 = 1;
const PROFILE: u8 = 1;
const PACKET_BOUND: usize = MAX_PACKET_BYTES + 2;
// At most two frames are needed to flush a partial frame plus lookahead.
const TAIL_RESERVE: usize = 2 * PACKET_BOUND;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceNote {
    pub bytes: Vec<u8>,
    pub sample_count: u32,
    pub waveform: Vec<u8>,
}

struct Parsed {
    sample_count: u32,
    pre_skip: u32,
    packets: Vec<Range<usize>>,
}

fn parse_layout(bytes: &[u8]) -> Result<Parsed, String> {
    if bytes.len() < HEADER_BYTES || bytes.len() > MAX_CONTAINER_BYTES {
        return Err("Invalid voice container size".into());
    }
    if &bytes[..4] != MAGIC || bytes[4] != VERSION || bytes[5] != PROFILE {
        return Err("Unsupported voice container profile".into());
    }
    let sample_count = u32::from_le_bytes(bytes[6..10].try_into().unwrap());
    let pre_skip = u32::from_le_bytes(bytes[10..14].try_into().unwrap());
    if sample_count == 0
        || sample_count > MAX_SAMPLES
        || pre_skip == 0
        || pre_skip > FRAME_SAMPLES as u32
    {
        return Err("Invalid voice duration or pre-skip".into());
    }
    let packet_count = (sample_count as usize + pre_skip as usize).div_ceil(FRAME_SAMPLES);
    let mut packets = Vec::with_capacity(packet_count);
    let mut offset = HEADER_BYTES;
    for _ in 0..packet_count {
        // Packet lengths <= 4000 need at most two ULEB bytes. The second
        // group must be nonzero, and neither may carry another continuation.
        let first = *bytes.get(offset).ok_or("Missing voice packet")?;
        offset += 1;
        let length = if first & 0x80 == 0 {
            first as usize
        } else {
            let second = *bytes.get(offset).ok_or("Truncated voice packet length")?;
            offset += 1;
            if second == 0 || second & 0x80 != 0 {
                return Err("Noncanonical voice packet length".into());
            }
            (first as usize & 0x7f) | ((second as usize) << 7)
        };
        if length == 0 || length > MAX_PACKET_BYTES {
            return Err("Invalid voice packet length".into());
        }
        let end = offset
            .checked_add(length)
            .ok_or("Invalid voice packet length")?;
        let packet = bytes.get(offset..end).ok_or("Truncated voice packet")?;
        dmsg_opus_sys::validate_packet(packet)?;
        packets.push(offset..end);
        offset = end;
    }
    if offset != bytes.len() {
        return Err("Unexpected voice packet or trailing bytes".into());
    }
    Ok(Parsed {
        sample_count,
        pre_skip,
        packets,
    })
}

/// Validates the entire bounded container, including every packet's structure,
/// mono channel count, bandwidth, 20ms duration and exact total packet count.
pub fn parse(bytes: &[u8]) -> Result<VoiceNote, String> {
    let parsed = parse_layout(bytes)?;
    Ok(VoiceNote {
        bytes: bytes.to_vec(),
        sample_count: parsed.sample_count,
        waveform: bytes[14..HEADER_BYTES].to_vec(),
    })
}

fn append_packet(body: &mut Vec<u8>, packet: &[u8]) -> Result<(), String> {
    let length_bytes = if packet.len() < 128 { 1 } else { 2 };
    if HEADER_BYTES + body.len() + length_bytes + packet.len() > MAX_CONTAINER_BYTES {
        return Err("Voice container size limit reached".into());
    }
    if length_bytes == 1 {
        body.push(packet.len() as u8);
    } else {
        body.push((packet.len() as u8 & 0x7f) | 0x80);
        body.push((packet.len() >> 7) as u8);
    }
    body.extend_from_slice(packet);
    Ok(())
}

pub struct VoiceEncoder {
    encoder: Encoder,
    pre_skip: u32,
    sample_count: u32,
    body: Vec<u8>,
    pending: [i16; FRAME_SAMPLES],
    pending_len: usize,
    peaks: Vec<u16>,
}

impl VoiceEncoder {
    pub fn new() -> Result<Self, String> {
        let encoder = Encoder::new()?;
        let pre_skip = encoder.lookahead()?;
        if pre_skip == 0 || pre_skip > FRAME_SAMPLES as u32 {
            return Err("Unsupported Opus lookahead".into());
        }
        Ok(Self {
            encoder,
            pre_skip,
            sample_count: 0,
            body: Vec::new(),
            pending: [0; FRAME_SAMPLES],
            pending_len: 0,
            peaks: Vec::new(),
        })
    }

    /// Accepts up to the first duration/encoded-size limit and returns the
    /// TOTAL accepted sample count. Arbitrary partial frames are supported.
    pub fn push(&mut self, pcm: &[i16]) -> Result<u32, String> {
        let mut offset = 0;
        while offset < pcm.len() && !self.at_limit() {
            let take = (FRAME_SAMPLES - self.pending_len)
                .min(pcm.len() - offset)
                .min((MAX_SAMPLES - self.sample_count) as usize);
            self.pending[self.pending_len..self.pending_len + take]
                .copy_from_slice(&pcm[offset..offset + take]);
            self.pending_len += take;
            self.sample_count += take as u32;
            offset += take;
            if self.pending_len == FRAME_SAMPLES {
                let packet = self.encoder.encode(&self.pending)?;
                append_packet(&mut self.body, &packet)?;
                self.peaks.push(peak(&self.pending));
                self.pending.fill(0);
                self.pending_len = 0;
            }
        }
        Ok(self.sample_count)
    }

    /// Flushes a copy of the current codec state, leaving recording resumable.
    /// No PCM history is retained: only the current partial frame and peaks.
    pub fn snapshot(&self) -> Result<VoiceNote, String> {
        self.finalize(self.encoder.copy()?, self.body.clone())
    }

    pub fn finish(self) -> Result<VoiceNote, String> {
        // Use the same non-mutating finalizer as snapshots. Moving the live
        // state into it avoids an encoder-state copy on the send path.
        let Self {
            encoder,
            pre_skip,
            sample_count,
            body,
            pending,
            pending_len,
            peaks,
        } = self;
        finalize(
            encoder,
            pre_skip,
            sample_count,
            body,
            &pending[..pending_len],
            &peaks,
        )
    }

    fn finalize(&self, encoder: Encoder, body: Vec<u8>) -> Result<VoiceNote, String> {
        finalize(
            encoder,
            self.pre_skip,
            self.sample_count,
            body,
            &self.pending[..self.pending_len],
            &self.peaks,
        )
    }

    pub fn at_limit(&self) -> bool {
        self.sample_count == MAX_SAMPLES
            || (self.pending_len == 0
                && HEADER_BYTES + self.body.len() + PACKET_BOUND + TAIL_RESERVE
                    > MAX_CONTAINER_BYTES)
    }
}

fn peak(pcm: &[i16]) -> u16 {
    pcm.iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap_or(0)
}

fn waveform(peaks: &[u16], pending: &[i16]) -> Vec<u8> {
    let mut all = peaks.to_vec();
    if !pending.is_empty() {
        all.push(peak(pending));
    }
    (0..WAVEFORM_BYTES)
        .map(|bin| {
            let start = bin * all.len() / WAVEFORM_BYTES;
            let end = ((bin + 1) * all.len())
                .div_ceil(WAVEFORM_BYTES)
                .min(all.len());
            let amplitude = all[start..end].iter().copied().max().unwrap_or(0);
            ((amplitude as u32 * 255) / 32768) as u8
        })
        .collect()
}

fn finalize(
    mut encoder: Encoder,
    pre_skip: u32,
    sample_count: u32,
    mut body: Vec<u8>,
    pending: &[i16],
    peaks: &[u16],
) -> Result<VoiceNote, String> {
    if sample_count == 0 {
        return Err("Voice note is empty".into());
    }
    let wave = waveform(peaks, pending);
    let completed_frames = sample_count as usize / FRAME_SAMPLES;
    let required_frames = (sample_count as usize + pre_skip as usize).div_ceil(FRAME_SAMPLES);
    let mut frame = [0; FRAME_SAMPLES];
    frame[..pending.len()].copy_from_slice(pending);
    for _ in completed_frames..required_frames {
        append_packet(&mut body, &encoder.encode(&frame)?)?;
        frame.fill(0);
    }
    let mut bytes = Vec::with_capacity(HEADER_BYTES + body.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&[VERSION, PROFILE]);
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    bytes.extend_from_slice(&pre_skip.to_le_bytes());
    bytes.extend_from_slice(&wave);
    bytes.extend_from_slice(&body);
    Ok(VoiceNote {
        bytes,
        sample_count,
        waveform: wave,
    })
}

pub struct VoiceDecoder {
    decoder: Decoder,
    bytes: Vec<u8>,
    parsed: Parsed,
    packet_index: usize,
    frame: [i16; FRAME_SAMPLES],
    frame_offset: usize,
    skip_left: usize,
    position: u32,
}

impl VoiceDecoder {
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        let parsed = parse_layout(bytes)?;
        let skip_left = parsed.pre_skip as usize;
        Ok(Self {
            decoder: Decoder::new()?,
            bytes: bytes.to_vec(),
            parsed,
            packet_index: 0,
            frame: [0; FRAME_SAMPLES],
            frame_offset: FRAME_SAMPLES,
            skip_left,
            position: 0,
        })
    }

    /// Reads at most one 100ms batch, trims lookahead and final padding, and
    /// returns an empty vector at end. Invalid data is never replaced with PLC.
    pub fn read(&mut self, max_samples: usize) -> Result<Vec<i16>, String> {
        if max_samples > MAX_BATCH_SAMPLES {
            return Err("Voice PCM batch is too large".into());
        }
        let count = max_samples.min((self.parsed.sample_count - self.position) as usize);
        let mut result = Vec::with_capacity(count);
        while result.len() < count {
            if self.frame_offset == FRAME_SAMPLES {
                let range = self
                    .parsed
                    .packets
                    .get(self.packet_index)
                    .ok_or("Missing voice packet")?;
                self.frame = self.decoder.decode(&self.bytes[range.clone()])?;
                self.packet_index += 1;
                self.frame_offset = self.skip_left.min(FRAME_SAMPLES);
                self.skip_left -= self.frame_offset;
            }
            let take = (count - result.len()).min(FRAME_SAMPLES - self.frame_offset);
            result.extend_from_slice(&self.frame[self.frame_offset..self.frame_offset + take]);
            self.frame_offset += take;
            self.position += take as u32;
        }
        Ok(result)
    }

    /// Replays from the beginning so predictive/NoLACE state is preserved.
    /// The caller runs this bounded (<=60s) operation off the UI thread.
    pub fn seek(&mut self, sample: u32) -> Result<(), String> {
        if sample > self.parsed.sample_count {
            return Err("Invalid voice seek position".into());
        }
        self.decoder = Decoder::new()?;
        self.packet_index = 0;
        self.frame_offset = FRAME_SAMPLES;
        self.skip_left = self.parsed.pre_skip as usize;
        self.position = 0;
        while self.position < sample {
            self.read(((sample - self.position) as usize).min(MAX_BATCH_SAMPLES))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic voiced, band-limited fixture long enough to warm up the
    // stateful NoLACE network. No external files/downloads or quality scoring.
    fn speech_fixture(count: usize) -> Vec<i16> {
        (0..count)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                let frequency = 120.0 + 15.0 * (t * 2.1).sin();
                let envelope = 0.6 + 0.35 * (t * 7.0).sin();
                let value = (1..=12)
                    .map(|h| {
                        let phase = std::f64::consts::TAU * frequency * h as f64 * t;
                        phase.sin() / h as f64
                    })
                    .sum::<f64>();
                (value * envelope * 9000.0) as i16
            })
            .collect()
    }

    fn encode(pcm: &[i16], batch: usize) -> VoiceNote {
        let mut encoder = VoiceEncoder::new().unwrap();
        for chunk in pcm.chunks(batch) {
            encoder.push(chunk).unwrap();
        }
        encoder.finish().unwrap()
    }

    fn decode(bytes: &[u8], batch: usize) -> Vec<i16> {
        let mut decoder = VoiceDecoder::new(bytes).unwrap();
        let mut pcm = Vec::new();
        loop {
            let part = decoder.read(batch).unwrap();
            if part.is_empty() {
                break;
            }
            pcm.extend(part);
        }
        pcm
    }

    #[test]
    fn exact_trim_partial_frames_and_lookahead() {
        for count in [1, 103, 216, 319, 320, 321, 640, 1703, 16_007] {
            let note = encode(&speech_fixture(count), 137);
            assert_eq!(note.sample_count as usize, count);
            assert_eq!(parse(&note.bytes).unwrap(), note);
            assert_eq!(decode(&note.bytes, 117).len(), count);
            let skip = u32::from_le_bytes(note.bytes[10..14].try_into().unwrap());
            assert_eq!(skip, Encoder::new().unwrap().lookahead().unwrap());
            let parsed = parse_layout(&note.bytes).unwrap();
            assert_eq!(
                parsed.packets.len(),
                (count + skip as usize).div_ceil(FRAME_SAMPLES)
            );
            let mut raw = Decoder::new().unwrap();
            let untrimmed: Vec<i16> = parsed
                .packets
                .iter()
                .flat_map(|range| raw.decode(&note.bytes[range.clone()]).unwrap())
                .collect();
            assert_eq!(
                decode(&note.bytes, 117),
                untrimmed[skip as usize..skip as usize + count]
            );
        }
    }

    #[test]
    fn snapshots_do_not_reset_live_state_and_batching_is_identical() {
        let pcm = speech_fixture(16_007);
        let expected = encode(&pcm, 1600);
        let mut encoder = VoiceEncoder::new().unwrap();
        encoder.push(&pcm[..1703]).unwrap();
        let snapshot = encoder.snapshot().unwrap();
        assert_eq!(snapshot, encoder.snapshot().unwrap());
        assert_eq!(decode(&snapshot.bytes, 1600).len(), 1703);
        assert_eq!(snapshot, encode(&pcm[..1703], 47));
        encoder.push(&pcm[1703..]).unwrap();
        assert_eq!(encoder.finish().unwrap(), expected);
        assert_eq!(encode(&pcm, 13), expected);
    }

    #[test]
    fn seek_and_read_preserve_continuous_decoder_state() {
        let note = encode(&speech_fixture(32_017), 1600);
        let expected = decode(&note.bytes, 1600);
        assert_eq!(decode(&note.bytes, 1), expected);
        let mut decoder = VoiceDecoder::new(&note.bytes).unwrap();
        for at in [7311, 0, 315, 32_017, 31_999] {
            decoder.seek(at).unwrap();
            let got = decoder.read(1600).unwrap();
            assert_eq!(
                got,
                expected[at as usize..(at as usize + 1600).min(expected.len())]
            );
        }
        assert!(decoder.seek(32_018).is_err());
        assert!(decoder.read(1601).is_err());
    }

    #[test]
    fn rejects_missing_extra_corrupt_and_noncanonical_packets() {
        let note = encode(&speech_fixture(640), 320);
        for length in 0..note.bytes.len() {
            assert!(parse(&note.bytes[..length]).is_err());
        }
        let mut bad = note.bytes.clone();
        bad.push(0);
        assert!(parse(&bad).is_err());
        for index in [0, 4, 5] {
            let mut bad = note.bytes.clone();
            bad[index] ^= 0xff;
            assert!(parse(&bad).is_err());
        }
        for count in [0, MAX_SAMPLES + 1] {
            let mut bad = note.bytes.clone();
            bad[6..10].copy_from_slice(&count.to_le_bytes());
            assert!(parse(&bad).is_err());
        }
        for skip in [0u32, 321] {
            let mut bad = note.bytes.clone();
            bad[10..14].copy_from_slice(&skip.to_le_bytes());
            assert!(parse(&bad).is_err());
        }
        let mut bad = note.bytes.clone();
        bad[HEADER_BYTES] = 0;
        assert!(parse(&bad).is_err());
        let mut bad = note.bytes.clone();
        bad.splice(HEADER_BYTES..HEADER_BYTES + 1, [0x81, 0]);
        assert!(parse(&bad).is_err());
        let mut bad = note.bytes.clone();
        bad.splice(HEADER_BYTES..HEADER_BYTES + 1, [0xff, 0xff, 0x01]);
        assert!(parse(&bad).is_err());
        let mut bad = note.bytes.clone();
        bad.splice(HEADER_BYTES..HEADER_BYTES + 1, [0xa1, 0x1f]); // canonical 4001
        assert!(parse(&bad).is_err());
        let layout = parse_layout(&note.bytes).unwrap();
        let mut bad = note.bytes.clone();
        bad[layout.packets[0].start] |= 4; // stereo TOC
        assert!(parse(&bad).is_err());
        let mut bad = note.bytes.clone();
        bad[layout.packets[0].start] = 0x58; // 60ms SILK WB instead of 20ms
        assert!(parse(&bad).is_err());
        assert!(dmsg_opus_sys::validate_packet(&[0x48]).is_err()); // empty SILK frame: no PLC
        assert!(dmsg_opus_sys::validate_packet(&[0xff]).is_err());
        assert!(parse(&vec![0; MAX_CONTAINER_BYTES + 1]).is_err());
    }

    #[test]
    fn duration_and_finalized_size_limits_and_empty_note() {
        let mut encoder = VoiceEncoder::new().unwrap();
        assert!(encoder.snapshot().is_err());
        assert!(VoiceEncoder::new().unwrap().finish().is_err());
        let silence = [0; MAX_BATCH_SAMPLES];
        for _ in 0..600 {
            encoder.push(&silence).unwrap();
        }
        assert!(encoder.at_limit());
        assert_eq!(encoder.push(&[1; 320]).unwrap(), MAX_SAMPLES);
        let note = encoder.finish().unwrap();
        assert_eq!(note.sample_count, MAX_SAMPLES);
        assert!(note.bytes.len() <= MAX_CONTAINER_BYTES);
        assert_eq!(decode(&note.bytes, 1600).len(), MAX_SAMPLES as usize);
        assert_eq!(note.waveform, vec![0; 64]);
        // Full-scale deterministic noise exercises unconstrained VBR's
        // variable packet sizes at the duration bound without a PCM history.
        let mut encoder = VoiceEncoder::new().unwrap();
        let mut state = 1u32;
        let mut noise = [0; MAX_BATCH_SAMPLES];
        for _ in 0..600 {
            for sample in &mut noise {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *sample = (state >> 16) as i16;
            }
            encoder.push(&noise).unwrap();
        }
        assert!(encoder.at_limit());
        let accepted = encoder.push(&noise).unwrap();
        let note = encoder.finish().unwrap();
        assert_eq!(note.sample_count, accepted);
        assert!(note.bytes.len() <= MAX_CONTAINER_BYTES);
        assert_eq!(parse(&note.bytes).unwrap(), note);
        assert_eq!(decode(&note.bytes, 1600).len(), accepted as usize);
    }

    #[test]
    fn bundled_nolace_activates_after_warmup_on_identical_packets() {
        let note = encode(&speech_fixture(5 * SAMPLE_RATE as usize), 1600);
        let layout = parse_layout(&note.bytes).unwrap();
        let mut base = Decoder::with_complexity(0).unwrap();
        let mut enhanced = Decoder::new().unwrap();
        let mut differing_samples = 0;
        let mut supported_packets = 0;
        for (index, range) in layout.packets.iter().enumerate() {
            let packet = &note.bytes[range.clone()];
            if packet[0] >> 3 == 9 {
                supported_packets += 1;
            } // SILK WB,20ms
            let plain = base.decode(packet).unwrap();
            let nolace = enhanced.decode(packet).unwrap();
            if index >= 50 {
                differing_samples += plain.iter().zip(nolace).filter(|(a, b)| **a != *b).count();
            }
        }
        assert!(
            supported_packets > 100,
            "fixture must exercise supported SILK WB"
        );
        assert!(
            differing_samples > 1000,
            "NoLACE must change actual decoded PCM after warm-up"
        );
    }
}
