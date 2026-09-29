//! K4: генерация Kotlin-биндингов UniFFI из host-cdylib.
//!
//! Запуск: `DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings`.
//! Требует собранный `target/debug/libdmsg_core.so` (`cargo build -p dmsg-core`).
//! Пишет `android/app/src/main/java/uniffi/dmsg_core.kt` — единственный
//! сгенерированный файл в репо; правится только перегенерацией, не руками.
//! Обычный `cargo test` его не трогает (env-gate).

use camino::{Utf8Path, Utf8PathBuf};

#[test]
fn gen_kotlin_bindings() {
    if std::env::var("DMSG_GEN_BINDINGS").as_deref() != Ok("1") {
        return;
    }
    let manifest = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap().to_owned();
    // Host-cdylib: собран обычным cargo build (не cargo-ndk).
    let profile = std::env::var("DMSG_PROFILE").unwrap_or_else(|_| "debug".into());
    let lib = workspace.join(format!("target/{profile}/libdmsg_core.so"));
    assert!(lib.exists(), "build host cdylib first: cargo build -p dmsg-core");
    let out_dir: Utf8PathBuf =
        workspace.join("android/app/src/main/java").to_owned();
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let out: &Utf8Path = out_dir.as_path();
    uniffi_bindgen::library_mode::generate_bindings(
        &lib,
        Some("dmsg_core".into()),
        &uniffi_bindgen::bindings::KotlinBindingGenerator,
        &uniffi_bindgen::EmptyCrateConfigSupplier,
        None,
        out,
        false,
    )
    .expect("generate kotlin bindings");
    // UniFFI templates contain trailing spaces; normalize deterministically so
    // regenerated tracked bindings pass the repository's diff whitespace gate.
    let generated = out.join("uniffi/dmsg_core/dmsg_core.kt");
    let source = std::fs::read_to_string(&generated).expect("read generated bindings");
    let normalized = source.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(&generated, normalized).expect("normalize generated bindings");
    assert!(
        out.join("uniffi/dmsg_core/dmsg_core.kt").exists(),
        "dmsg_core.kt missing after generate"
    );
}
