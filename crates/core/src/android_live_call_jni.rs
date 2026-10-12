//! Production PCM JNI. Handles come only from an authenticated call actor.
use crate::{call_media_handles as handles, live_media::CaptureTimestamp};
use jni::{
    objects::{JObject, JShortArray},
    sys::{jboolean, jint, jlong, jstring},
    JNIEnv,
};
use std::time::Instant;
use zeroize::Zeroize;

fn json(env: &mut JNIEnv<'_>, value: Result<crate::live_media::Stats, String>) -> jstring {
    match value.and_then(|s| serde_json::to_string(&s).map_err(|_| "call stats unavailable".into()))
    {
        Ok(value) => env
            .new_string(value)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(_) => {
            let _ = env.throw_new(
                "java/lang/IllegalStateException",
                "retired or unavailable call media",
            );
            std::ptr::null_mut()
        }
    }
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_push(
    mut env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
    pcm: JShortArray<'_>,
    valid: jint,
    read_position: jlong,
    timestamp_frame: jlong,
    timestamp_ns: jlong,
) -> jboolean {
    if valid <= 0 || valid > 160 || !env.get_array_length(&pcm).is_ok_and(|n| n >= valid) {
        return 0;
    }
    let Some(entry) = handles::get(handle) else {
        return 0;
    };
    let started = Instant::now();
    let observed_ns = env
        .call_static_method("java/lang/System", "nanoTime", "()J", &[])
        .and_then(|v| v.j())
        .unwrap_or(-1);
    let completed = Instant::now();
    let mut samples = [0i16; 160];
    if env
        .get_short_array_region(&pcm, 0, &mut samples[..valid as usize])
        .is_err()
    {
        samples.zeroize();
        return 0;
    }
    let accepted = entry.audio.push_recorded_observed(
        &samples[..valid as usize],
        CaptureTimestamp {
            read_position,
            frame_position: timestamp_frame,
            nano_time: timestamp_ns,
            observed_ns,
        },
        started,
        completed,
    );
    samples.zeroize();
    u8::from(accepted)
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_pull(
    env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
    pcm: JShortArray<'_>,
    valid: jint,
) -> jint {
    if valid <= 0 || valid > 160 || !env.get_array_length(&pcm).is_ok_and(|n| n >= valid) {
        return 0;
    }
    let Some(entry) = handles::get(handle) else {
        return 0;
    };
    let mut samples = [0i16; 160];
    let count = entry.audio.pull(&mut samples[..valid as usize]);
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
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_sinkQueued(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
    samples: jint,
) {
    if (0..=640).contains(&samples) {
        if let Some(entry) = handles::get(handle) {
            entry.audio.sink_queued(samples as usize);
        }
    }
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_sinkRate(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
    ppb: jlong,
) -> jboolean {
    u8::from(handles::get(handle).is_some_and(|e| e.audio.sink_rate(ppb)))
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_renderExpiredSamples(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
) -> jlong {
    handles::get(handle)
        .and_then(|e| e.audio.render_expired_samples())
        .and_then(|n| jlong::try_from(n).ok())
        .unwrap_or(-1)
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_stats(
    mut env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
) -> jstring {
    json(
        &mut env,
        handles::get(handle)
            .map(|e| e.audio.snapshot())
            .ok_or_else(|| "retired call handle".into()),
    )
}
#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_CallMediaJni_stop(
    mut env: JNIEnv<'_>,
    _object: JObject<'_>,
    handle: jlong,
) -> jstring {
    json(&mut env, handles::retire(handle))
}
