use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=opus-1.6.1");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let mut build = cmake::Config::new(package.join("opus-1.6.1"));
    build
        .profile("Release")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .define("OPUS_BUILD_SHARED_LIBRARY", "OFF")
        .define("OPUS_BUILD_TESTING", "OFF")
        .define("OPUS_BUILD_PROGRAMS", "OFF")
        .define("OPUS_FIXED_POINT", "OFF")
        .define("OPUS_DRED", "OFF")
        .define("OPUS_OSCE", "ON")
        .define("OPUS_HARDENING", "ON");
    // The official release includes generated inline models. Its CMake has no
    // BWE/QEXT switches: neither feature is enabled by the upstream defaults.
    if target.ends_with("android") {
        let ndk =
            PathBuf::from(env::var_os("ANDROID_NDK_HOME").expect("ANDROID_NDK_HOME required"));
        let abi = match target.as_str() {
            "aarch64-linux-android" => "arm64-v8a",
            "x86_64-linux-android" => "x86_64",
            _ => panic!("unsupported Android ABI"),
        };
        build
            .define(
                "CMAKE_TOOLCHAIN_FILE",
                ndk.join("build/cmake/android.toolchain.cmake"),
            )
            .define("ANDROID_ABI", abi)
            .define("ANDROID_PLATFORM", "android-26");
    }
    let artifacts = build.build();
    println!(
        "cargo:rustc-link-search=native={}",
        artifacts.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=opus");
    println!("cargo:rustc-link-lib=m");
}
