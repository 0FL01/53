use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=libsrtp-2.7.0");
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let mut build = cmake::Config::new(package.join("native"));
    build
        .profile("Release")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("CMAKE_C_VISIBILITY_PRESET", "hidden")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("LIBSRTP_TEST_APPS", "OFF")
        .define("ENABLE_OPENSSL", "OFF")
        .define("ENABLE_MBEDTLS", "OFF")
        .define("ENABLE_NSS", "OFF")
        .define("ENABLE_DEBUG_LOGGING", "OFF")
        .define("ERR_REPORTING_STDOUT", "OFF")
        .define("ERR_REPORTING_FILE", "");
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
    println!("cargo:rustc-link-lib=static=dmsg_srtp_shim");
    println!("cargo:rustc-link-lib=static=srtp2");
}
