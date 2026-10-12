use dmsg_opus_sys::live::LiveProfile;
use zeroize::Zeroize;

/// One already-negotiated directional media epoch. No carrier or relay identity.
pub struct MediaConfig {
    pub profile_ms: u16,
    pub media_tx: [u8; 30],
    pub media_rx: [u8; 30],
    pub ssrc_tx: u32,
    pub ssrc_rx: u32,
    pub cname: [u8; 8],
    pub peer_cname: [u8; 8],
    pub initial_sequence: u16,
    pub peer_initial_sequence: u16,
    pub initial_timestamp: u32,
    pub peer_initial_timestamp: u32,
    pub healthy_cycle_ms: u32,
    pub capacity_bps: u32,
}

impl MediaConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        profile(self.profile_ms)?;
        if self.ssrc_tx == 0
            || self.ssrc_rx == 0
            || self.ssrc_tx == self.ssrc_rx
            || self.media_tx == self.media_rx
            || self.media_tx == [0; 30]
            || self.media_rx == [0; 30]
            || !(200..=5000).contains(&self.healthy_cycle_ms)
            || !(50_000..=80_000).contains(&self.capacity_bps)
            || self.cname == self.peer_cname
            || !self
                .cname
                .iter()
                .chain(&self.peer_cname)
                .all(|b| b.is_ascii_graphic())
        {
            return Err("invalid media binding".into());
        }
        Ok(())
    }
}

impl Drop for MediaConfig {
    fn drop(&mut self) {
        self.media_tx.zeroize();
        self.media_rx.zeroize();
    }
}

pub(crate) fn profile(ms: u16) -> Result<LiveProfile, String> {
    match ms {
        #[cfg(any(feature = "voice-probe", test))]
        20 => Ok(LiveProfile::Ms20),
        40 => Ok(LiveProfile::Ms40),
        #[cfg(any(feature = "voice-probe", test))]
        60 => Ok(LiveProfile::Ms60),
        _ => Err("invalid probe duration".into()),
    }
}
