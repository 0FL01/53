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
    final_stats: Mutex<Option<Completed>>,
    joined: Condvar,
}
struct Completed {
    stats: Stats,
    native_transport: Option<serde_json::Value>,
}
impl Entry {
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub(crate) fn complete(&self, stats: Stats) {
        // The actor has joined media, but DNS/service teardown and JNI handle
        // retirement may follow. Freeze even an unavailable observation here.
        self.complete_with_transport(stats, crate::dns::transport_evidence(&self.owner));
    }
    fn complete_with_transport(&self, stats: Stats, native_transport: Option<serde_json::Value>) {
        if let Ok(mut final_stats) = self.final_stats.lock() {
            *final_stats = Some(Completed {
                stats,
                native_transport,
            });
            self.joined.notify_all();
        }
    }

    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn native_transport(&self) -> Option<serde_json::Value> {
        {
            let final_stats = self.final_stats.try_lock().ok()?;
            if let Some(completed) = final_stats.as_ref() {
                return completed.native_transport.clone();
            }
        }
        crate::dns::transport_evidence(&self.owner)
    }

    // Only the existing JNI JSON is extended; Stats and the host retirement
    // contract stay unchanged. Also used by the pure retirement tests below.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(crate) fn stats_json(&self, stats: &Stats) -> Result<String, String> {
        let mut value = serde_json::to_value(stats).map_err(|_| "call stats unavailable")?;
        value
            .as_object_mut()
            .ok_or("call stats unavailable")?
            .insert(
                "native_transport".into(),
                self.native_transport().unwrap_or(serde_json::Value::Null),
            );
        serde_json::to_string(&value).map_err(|_| "call stats unavailable".into())
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
        .stats
        .clone();
    drop(final_stats);
    handles()
        .lock()
        .map_err(|_| "call handle owner unavailable")?
        .entries
        .remove(&id);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_evidence_final_json_survives_retirement_and_owner_reuse() {
        let owner = "diagnostic-retirement-fixture";
        let (port, _capture) = crate::live_media::session::new_port();
        port.finish(Ok(()));
        let (id, entry) = register(owner, port.audio()).unwrap();
        let stats = port.snapshot();
        let live: serde_json::Value =
            serde_json::from_str(&entry.stats_json(&stats).unwrap()).unwrap();
        assert!(live["native_transport"].is_null());
        let final_evidence = serde_json::json!({
            "publication_age_us": null,
            "age_since_observed_ms": 7,
            "native_capture": {"now_native_us": 41, "paths": []},
        });
        entry.complete_with_transport(stats, Some(final_evidence.clone()));
        // This retained Arc is the same lifetime pattern as JNI stop: remove
        // the handle first, then serialize its frozen completion observation.
        let retained = get(id).unwrap();
        let retired = retire(id).unwrap();
        assert!(get(id).is_none());
        assert!(retired.stopped);
        let mut value: serde_json::Value =
            serde_json::from_str(&retained.stats_json(&retired).unwrap()).unwrap();
        assert_eq!(
            value.as_object_mut().unwrap().remove("native_transport"),
            Some(final_evidence)
        );
        assert_eq!(
            value,
            serde_json::to_value(&retired).unwrap(),
            "only one JNI JSON key is added"
        );

        let (next_id, next_entry) = register(owner, port.audio()).unwrap();
        next_entry.complete(retired.clone()); // No DNS run: freeze explicit null.
        retire(next_id).unwrap();
        let next: serde_json::Value =
            serde_json::from_str(&next_entry.stats_json(&retired).unwrap()).unwrap();
        assert!(next["native_transport"].is_null());
        let previous: serde_json::Value =
            serde_json::from_str(&retained.stats_json(&retired).unwrap()).unwrap();
        assert_eq!(
            previous["native_transport"]["native_capture"]["now_native_us"],
            41
        );
    }
}
