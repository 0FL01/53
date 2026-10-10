//! Opt-in, memory-only M1-V fixture. No DB, account API, Olm or production calls.
mod clock;
mod codec;
pub mod fixture;
mod lane;
mod packet;
pub mod relay;
pub mod service;
mod session;

#[cfg(target_os = "android")]
pub(crate) use session::{AudioPort, CaptureTimestamp};
pub use session::{Probe, Stats, DECODE_WRITE_PREPARATION_MS};

use dmsg_opus_sys::live::{LiveDecoder, LiveEncoder, LiveProfile};
use serde::Serialize;

#[derive(Serialize)]
pub struct CodecEvidence {
    pub nolace_changed_samples: usize,
    pub deep_plc_changed_samples: usize,
    pub wideband_packets: usize,
    pub lookahead_samples: u32,
}

/// Deterministic model-path evidence in the actually linked binary, not MOS.
pub fn codec_evidence() -> Result<CodecEvidence, String> {
    let profile = LiveProfile::Ms40;
    let mut encoder = LiveEncoder::new(profile)?;
    let lookahead_samples = encoder.lookahead()?;
    let mut base = LiveDecoder::with_complexity(profile, 0)?;
    let mut enhanced = LiveDecoder::with_complexity(profile, 7)?;
    let mut without_plc = LiveDecoder::with_complexity(profile, 4)?;
    let mut with_plc = LiveDecoder::with_complexity(profile, 5)?;
    let mut evidence = CodecEvidence {
        nolace_changed_samples: 0,
        deep_plc_changed_samples: 0,
        wideband_packets: 0,
        lookahead_samples,
    };
    for frame in 0..100 {
        let pcm = test_tone(frame * 640, 640);
        let packet = encoder.encode(&pcm)?;
        if packet.info.bandwidth == 1103 {
            evidence.wideband_packets += 1;
        }
        let a = base.decode(&packet.bytes)?;
        let b = enhanced.decode(&packet.bytes)?;
        if frame >= 50 {
            evidence.nolace_changed_samples += a.iter().zip(&b).filter(|(a, b)| a != b).count();
        }
        without_plc.decode(&packet.bytes)?;
        with_plc.decode(&packet.bytes)?;
    }
    for _ in 0..3 {
        let a = without_plc.conceal()?;
        let b = with_plc.conceal()?;
        evidence.deep_plc_changed_samples += a.iter().zip(&b).filter(|(a, b)| a != b).count();
    }
    if evidence.wideband_packets <= 50
        || evidence.nolace_changed_samples == 0
        || evidence.deep_plc_changed_samples == 0
    {
        return Err("packaged neural decoder path not established".into());
    }
    Ok(evidence)
}

/// Authorized synthetic test signal, never represented as human listening proof.
pub fn test_tone(position: usize, count: usize) -> Vec<i16> {
    (position..position + count)
        .map(|i| {
            let phase = i as f64 * std::f64::consts::TAU * 120.0 / 16000.0;
            (phase.sin() * 8000.0 + (phase * 2.0).sin() * 3500.0 + (phase * 3.0).sin() * 1500.0)
                as i16
        })
        .collect()
}
