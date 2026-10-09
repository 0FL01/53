//! Debug-feature JNI: opaque finite probe handles, never PCM through UniFFI.
use crate::voice_probe::{AudioPort, CaptureTimestamp, Probe};
use jni::{
    objects::{JClass, JShortArray, JString},
    sys::{jboolean, jint, jlong, jstring},
    JNIEnv,
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use zeroize::Zeroize;

struct Handles {
    next: i64,
    retiring: bool,
    entries: BTreeMap<i64, Arc<Entry>>,
}
struct Entry {
    owner: Mutex<Probe>,
    audio: AudioPort,
}
fn handles() -> &'static Mutex<Handles> {
    static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
    HANDLES.get_or_init(|| {
        Mutex::new(Handles {
            next: 1,
            retiring: false,
            entries: BTreeMap::new(),
        })
    })
}
fn exception(env: &mut JNIEnv<'_>, message: &str) {
    let _ = env.throw_new("java/lang/IllegalStateException", message);
}
fn string(env: &mut JNIEnv<'_>, value: &str) -> jstring {
    match env.new_string(value) {
        Ok(value) => value.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
fn get(handle: jlong) -> Option<Arc<Entry>> {
    handles().lock().ok()?.entries.get(&handle).cloned()
}
fn start(env: &mut JNIEnv<'_>, fixture: Option<String>) -> jlong {
    let Ok(mut handles) = handles().lock() else {
        exception(env, "probe handle state unavailable");
        return 0;
    };
    if handles.retiring || !handles.entries.is_empty() {
        exception(env, "probe already owned; stop and join first");
        return 0;
    }
    let probe = match fixture {
        Some(path) => Probe::start_dns(Path::new(&path)),
        None => Probe::start_local(),
    };
    match probe {
        Ok(probe) => {
            let handle = handles.next;
            let Some(next) = handle.checked_add(1) else {
                exception(env, "probe handles exhausted");
                return 0;
            };
            handles.next = next;
            let audio = probe.audio();
            handles.entries.insert(
                handle,
                Arc::new(Entry {
                    owner: Mutex::new(probe),
                    audio,
                }),
            );
            handle
        }
        Err(_) => {
            exception(env, "probe start failed; invalid private fixture or owner");
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_codecEvidence(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    match crate::voice_probe::codec_evidence()
        .and_then(|e| serde_json::to_string(&e).map_err(|_| "probe evidence failed".into()))
    {
        Ok(value) => string(&mut env, &value),
        Err(_) => {
            exception(&mut env, "packaged live codec activation failed");
            std::ptr::null_mut()
        }
    }
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_startLocal(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jlong {
    start(&mut env, None)
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_startDns(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    path: JString<'_>,
) -> jlong {
    let path: String = match env.get_string(&path) {
        Ok(value) => value.into(),
        Err(_) => return 0,
    };
    start(&mut env, Some(path))
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_push(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    pcm: JShortArray<'_>,
    valid: jint,
    read_position: jlong,
    timestamp_frame: jlong,
    timestamp_ns: jlong,
) -> jboolean {
    if valid <= 0 || valid > 160 || !env.get_array_length(&pcm).is_ok_and(|len| len >= valid) {
        return 0;
    }
    let Some(owner) = get(handle) else {
        return 0;
    };
    // Query AudioRecord's Java timebase, then pair its age with the completed
    // native observation. The bracket bounds uncertainty; entry time would
    // count acquisition twice when native waiting is added at dequeue.
    let observation_started = Instant::now();
    let observed_ns = env
        .call_static_method("java/lang/System", "nanoTime", "()J", &[])
        .and_then(|value| value.j())
        .unwrap_or(-1);
    let observation_completed = Instant::now();
    let mut samples = [0i16; 160];
    if env
        .get_short_array_region(&pcm, 0, &mut samples[..valid as usize])
        .is_err()
    {
        return 0;
    }
    let accepted = owner.audio.push_recorded_observed(
        &samples[..valid as usize],
        CaptureTimestamp {
            read_position,
            frame_position: timestamp_frame,
            nano_time: timestamp_ns,
            observed_ns,
        },
        observation_started,
        observation_completed,
    );
    samples.zeroize();
    u8::from(accepted)
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_pull(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    pcm: JShortArray<'_>,
    valid: jint,
) -> jint {
    if valid <= 0 || valid > 160 || !env.get_array_length(&pcm).is_ok_and(|len| len >= valid) {
        return 0;
    }
    let Some(owner) = get(handle) else {
        return 0;
    };
    let mut samples = [0i16; 160];
    let count = owner.audio.pull(&mut samples[..valid as usize]);
    let result = if env
        .set_short_array_region(&pcm, 0, &samples[..count])
        .is_ok()
    {
        count as jint
    } else {
        0
    };
    samples.zeroize();
    result
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_sinkQueued(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    samples: jint,
) {
    if !(0..=640).contains(&samples) {
        return;
    }
    if let Some(owner) = get(handle) {
        owner.audio.sink_queued(samples as usize);
    }
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_stats(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    let Some(owner) = get(handle) else {
        exception(&mut env, "retired probe handle");
        return std::ptr::null_mut();
    };
    let result = owner
        .owner
        .lock()
        .ok()
        .and_then(|probe| serde_json::to_string(&probe.snapshot()).ok());
    match result {
        Some(value) => string(&mut env, &value),
        None => {
            exception(&mut env, "probe stats unavailable");
            std::ptr::null_mut()
        }
    }
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallProbeJni_stop(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    let owner = {
        let Ok(mut handles) = handles().lock() else {
            exception(&mut env, "probe handles unavailable");
            return std::ptr::null_mut();
        };
        let Some(owner) = handles.entries.remove(&handle) else {
            exception(&mut env, "retired probe handle");
            return std::ptr::null_mut();
        };
        handles.retiring = true;
        owner
    };
    let result = owner
        .owner
        .lock()
        .ok()
        .and_then(|mut probe| serde_json::to_string(&probe.stop()).ok());
    if let Ok(mut handles) = handles().lock() {
        handles.retiring = false;
    }
    match result {
        Some(value) => string(&mut env, &value),
        None => {
            exception(&mut env, "probe stop failed");
            std::ptr::null_mut()
        }
    }
}
