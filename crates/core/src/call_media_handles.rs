//! Call-issued audio leases. The actor joins media; JNI retires only after PCM workers join.
use crate::live_media::{AudioPort, Stats};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
};

pub(crate) struct Entry {
    owner: String,
    pub(crate) audio: AudioPort,
    stop: AtomicBool,
    final_stats: Mutex<Option<Stats>>,
    joined: Condvar,
}
impl Entry {
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub(crate) fn complete(&self, stats: Stats) {
        if let Ok(mut final_stats) = self.final_stats.lock() {
            *final_stats = Some(stats);
            self.joined.notify_all();
        }
    }
}
struct Handles {
    next: i64,
    entries: BTreeMap<i64, Arc<Entry>>,
}
fn handles() -> &'static Mutex<Handles> {
    static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
    HANDLES.get_or_init(|| {
        Mutex::new(Handles {
            next: 1,
            entries: BTreeMap::new(),
        })
    })
}
pub(crate) fn register(owner: &str, audio: AudioPort) -> Result<(i64, Arc<Entry>), String> {
    let mut handles = handles()
        .lock()
        .map_err(|_| "call handle owner unavailable")?;
    if handles.entries.values().any(|e| e.owner == owner) {
        return Err("call audio not retired".into());
    }
    let id = handles.next;
    handles.next = id.checked_add(1).ok_or("call handles exhausted")?;
    let entry = Arc::new(Entry {
        owner: owner.into(),
        audio,
        stop: AtomicBool::new(false),
        final_stats: Mutex::new(None),
        joined: Condvar::new(),
    });
    handles.entries.insert(id, entry.clone());
    Ok((id, entry))
}
pub(crate) fn get(id: i64) -> Option<Arc<Entry>> {
    handles().lock().ok()?.entries.get(&id).cloned()
}
pub(crate) fn retire(id: i64) -> Result<Stats, String> {
    let entry = get(id).ok_or("retired call handle")?;
    entry.stop.store(true, Ordering::Release);
    let mut final_stats = entry
        .final_stats
        .lock()
        .map_err(|_| "call retirement unavailable")?;
    while final_stats.is_none() {
        final_stats = entry
            .joined
            .wait(final_stats)
            .map_err(|_| "call retirement unavailable")?;
    }
    let stats = final_stats
        .as_ref()
        .ok_or("call retirement unavailable")?
        .clone();
    drop(final_stats);
    handles()
        .lock()
        .map_err(|_| "call handle owner unavailable")?
        .entries
        .remove(&id);
    Ok(stats)
}
