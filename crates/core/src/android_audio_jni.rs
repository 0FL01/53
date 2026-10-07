//! Narrow memory-only audio JNI ABI. Java owns threads/audio/lifecycle; Rust
//! owns bounded opaque codec handles. No JNIEnv, Java array or Activity escapes
//! a call. Errors have fixed Java types/messages and never include audio data.

use crate::voice_codec::{
    parse, VoiceDecoder, VoiceEncoder, MAX_BATCH_SAMPLES, MAX_CONTAINER_BYTES,
};
use jni::{
    objects::{JByteArray, JClass, JShortArray},
    sys::{jboolean, jbyteArray, jint, jlong, jshortArray, JNI_FALSE, JNI_TRUE},
    JNIEnv,
};
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, OnceLock},
};

const MAX_HANDLES: usize = 8;

enum Handle {
    Encoder(Mutex<Option<VoiceEncoder>>),
    Decoder(Mutex<Option<VoiceDecoder>>),
}

#[derive(Default)]
struct Handles {
    next: jlong,
    entries: BTreeMap<jlong, Arc<Handle>>,
}

fn handles() -> &'static Mutex<Handles> {
    static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(Handles::default()))
}

enum JavaError {
    Argument,
    State,
    Capacity,
}
type Result<T> = std::result::Result<T, JavaError>;

fn invoke<T: Default>(env: &mut JNIEnv, operation: impl FnOnce(&mut JNIEnv) -> Result<T>) -> T {
    let result = catch_unwind(AssertUnwindSafe(|| operation(env)));
    match result {
        Ok(Ok(value)) => value,
        error => {
            let (class, message) = match error {
                Ok(Err(JavaError::Argument)) => (
                    "java/lang/IllegalArgumentException",
                    "Invalid voice note or PCM argument",
                ),
                Ok(Err(JavaError::Capacity)) => (
                    "java/lang/IllegalStateException",
                    "Voice codec handle limit reached",
                ),
                _ => (
                    "java/lang/IllegalStateException",
                    "Voice codec operation failed or handle is closed",
                ),
            };
            // Preserve an existing JNI exception, such as allocation failure.
            if !env.exception_check().unwrap_or(true) {
                let _ = env.throw_new(class, message);
            }
            T::default()
        }
    }
}

fn insert(handle: Handle) -> Result<jlong> {
    let mut table = handles().lock().map_err(|_| JavaError::State)?;
    if table.entries.len() >= MAX_HANDLES {
        return Err(JavaError::Capacity);
    }
    let id = table.next.checked_add(1).ok_or(JavaError::Capacity)?;
    table.next = id;
    table.entries.insert(id, Arc::new(handle));
    Ok(id)
}

fn lookup(id: jlong, encoder: bool, consume: bool) -> Result<Arc<Handle>> {
    if id <= 0 {
        return Err(JavaError::State);
    }
    let mut table = handles().lock().map_err(|_| JavaError::State)?;
    let handle = table.entries.get(&id).ok_or(JavaError::State)?;
    if matches!(handle.as_ref(), Handle::Encoder(_)) != encoder {
        return Err(JavaError::State);
    }
    if consume {
        table.entries.remove(&id).ok_or(JavaError::State)
    } else {
        Ok(Arc::clone(handle))
    }
}

fn with_encoder<T>(
    id: jlong,
    operation: impl FnOnce(&mut VoiceEncoder) -> std::result::Result<T, String>,
) -> Result<T> {
    let handle = lookup(id, true, false)?;
    let Handle::Encoder(encoder) = handle.as_ref() else {
        return Err(JavaError::State);
    };
    let mut encoder = encoder.lock().map_err(|_| JavaError::State)?;
    operation(encoder.as_mut().ok_or(JavaError::State)?).map_err(|_| JavaError::State)
}

fn with_decoder<T>(
    id: jlong,
    operation: impl FnOnce(&mut VoiceDecoder) -> std::result::Result<T, String>,
) -> Result<T> {
    let handle = lookup(id, false, false)?;
    let Handle::Decoder(decoder) = handle.as_ref() else {
        return Err(JavaError::State);
    };
    let mut decoder = decoder.lock().map_err(|_| JavaError::State)?;
    operation(decoder.as_mut().ok_or(JavaError::State)?).map_err(|_| JavaError::State)
}

fn note_bytes(env: &mut JNIEnv, array: &JByteArray) -> Result<Vec<u8>> {
    if array.is_null() {
        return Err(JavaError::Argument);
    }
    let size = env.get_array_length(array).map_err(|_| JavaError::State)?;
    if size <= 0 || size as usize > MAX_CONTAINER_BYTES {
        return Err(JavaError::Argument);
    }
    env.convert_byte_array(array).map_err(|_| JavaError::State)
}

fn byte_array(env: &mut JNIEnv, bytes: &[u8]) -> Result<jbyteArray> {
    env.byte_array_from_slice(bytes)
        .map(|array| array.into_raw())
        .map_err(|_| JavaError::State)
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderCreate(
    mut env: JNIEnv,
    _: JClass,
) -> jlong {
    invoke(&mut env, |_| {
        insert(Handle::Encoder(Mutex::new(Some(
            VoiceEncoder::new().map_err(|_| JavaError::State)?,
        ))))
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderPush(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
    pcm: JShortArray,
) -> jint {
    invoke(&mut env, |env| {
        if pcm.is_null() {
            return Err(JavaError::Argument);
        }
        let length = env.get_array_length(&pcm).map_err(|_| JavaError::State)?;
        if length < 0 || length as usize > MAX_BATCH_SAMPLES {
            return Err(JavaError::Argument);
        }
        let mut samples = [0i16; MAX_BATCH_SAMPLES];
        env.get_short_array_region(&pcm, 0, &mut samples[..length as usize])
            .map_err(|_| JavaError::State)?;
        with_encoder(handle, |encoder| encoder.push(&samples[..length as usize]))
            .map(|count| count as jint)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderAtLimit(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
) -> jboolean {
    invoke(&mut env, |_| {
        with_encoder(handle, |encoder| {
            Ok(if encoder.at_limit() {
                JNI_TRUE
            } else {
                JNI_FALSE
            })
        })
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderSnapshot(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
) -> jbyteArray {
    invoke(&mut env, |env| {
        let note = with_encoder(handle, |encoder| encoder.snapshot())?;
        byte_array(env, &note.bytes)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderFinish(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
) -> jbyteArray {
    invoke(&mut env, |env| {
        let handle = lookup(handle, true, true)?;
        let Handle::Encoder(encoder) = handle.as_ref() else {
            return Err(JavaError::State);
        };
        let encoder = encoder
            .lock()
            .map_err(|_| JavaError::State)?
            .take()
            .ok_or(JavaError::State)?;
        let note = encoder.finish().map_err(|_| JavaError::State)?;
        byte_array(env, &note.bytes)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeEncoderCancel(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
) {
    invoke(&mut env, |_| {
        let handle = lookup(handle, true, true)?;
        let Handle::Encoder(encoder) = handle.as_ref() else {
            return Err(JavaError::State);
        };
        encoder.lock().map_err(|_| JavaError::State)?.take();
        Ok(())
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeNoteSamples(
    mut env: JNIEnv,
    _: JClass,
    bytes: JByteArray,
) -> jint {
    invoke(&mut env, |env| {
        let note = parse(&note_bytes(env, &bytes)?).map_err(|_| JavaError::Argument)?;
        Ok(note.sample_count as jint)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeNoteWaveform(
    mut env: JNIEnv,
    _: JClass,
    bytes: JByteArray,
) -> jbyteArray {
    invoke(&mut env, |env| {
        let note = parse(&note_bytes(env, &bytes)?).map_err(|_| JavaError::Argument)?;
        byte_array(env, &note.waveform)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeDecoderCreate(
    mut env: JNIEnv,
    _: JClass,
    bytes: JByteArray,
) -> jlong {
    invoke(&mut env, |env| {
        let decoder =
            VoiceDecoder::new(&note_bytes(env, &bytes)?).map_err(|_| JavaError::Argument)?;
        insert(Handle::Decoder(Mutex::new(Some(decoder))))
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeDecoderRead(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
    max_samples: jint,
) -> jshortArray {
    invoke(&mut env, |env| {
        if max_samples < 0 || max_samples as usize > MAX_BATCH_SAMPLES {
            return Err(JavaError::Argument);
        }
        let samples = with_decoder(handle, |decoder| decoder.read(max_samples as usize))?;
        let array = env
            .new_short_array(samples.len() as jint)
            .map_err(|_| JavaError::State)?;
        env.set_short_array_region(&array, 0, &samples)
            .map_err(|_| JavaError::State)?;
        Ok(array.into_raw())
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeDecoderSeek(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
    sample: jint,
) {
    invoke(&mut env, |_| {
        if sample < 0 {
            return Err(JavaError::Argument);
        }
        // The pure API bounds against the original note length. Its error is
        // a Java argument error rather than exposing a Rust error string.
        let handle = lookup(handle, false, false)?;
        let Handle::Decoder(decoder) = handle.as_ref() else {
            return Err(JavaError::State);
        };
        let mut decoder = decoder.lock().map_err(|_| JavaError::State)?;
        decoder
            .as_mut()
            .ok_or(JavaError::State)?
            .seek(sample as u32)
            .map_err(|_| JavaError::Argument)
    })
}

#[no_mangle]
pub extern "system" fn Java_org_dmsg_client_VoiceCodecJni_nativeDecoderClose(
    mut env: JNIEnv,
    _: JClass,
    handle: jlong,
) {
    invoke(&mut env, |_| {
        let handle = lookup(handle, false, true)?;
        let Handle::Decoder(decoder) = handle.as_ref() else {
            return Err(JavaError::State);
        };
        decoder.lock().map_err(|_| JavaError::State)?.take();
        Ok(())
    })
}
