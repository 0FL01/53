use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=../../vendor/slipstream");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("source");
    assert!(
        Command::new("python3")
            .arg(package.join("native/stage.py"))
            .arg(package.join("../../vendor/slipstream"))
            .arg(&source)
            .arg(package.join("native/embedding.patch"))
            .status()
            .expect("python3 and git are required")
            .success(),
        "stage pinned transport"
    );
    let target = env::var("TARGET").unwrap();
    assert!(
        target == "x86_64-unknown-linux-gnu"
            || target == "aarch64-linux-android"
            || target == "x86_64-linux-android",
        "supported targets: Linux x86_64 and Android arm64/x86_64 API26+"
    );
    let ssl = openssl_src::Build::new().build();
    let mut build = cmake::Config::new(package.join("native"));
    build
        .env("PKG_CONFIG_LIBDIR", ssl.lib_dir().join("pkgconfig"))
        .env("PKG_CONFIG_PATH", "")
        .define("SLIPSTREAM_SOURCE", &source)
        .define("OPENSSL_ROOT_DIR", ssl.lib_dir().parent().unwrap())
        .define("OPENSSL_INCLUDE_DIR", ssl.include_dir())
        .define("OPENSSL_CRYPTO_LIBRARY", ssl.lib_dir().join("libcrypto.a"))
        .define("OPENSSL_SSL_LIBRARY", ssl.lib_dir().join("libssl.a"))
        .define("OPENSSL_USE_STATIC_LIBS", "TRUE")
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        .profile("Release");
    if target.ends_with("android") {
        let ndk =
            PathBuf::from(env::var_os("ANDROID_NDK_HOME").expect("ANDROID_NDK_HOME required"));
        build
            .define(
                "CMAKE_TOOLCHAIN_FILE",
                ndk.join("build/cmake/android.toolchain.cmake"),
            )
            .define(
                "ANDROID_ABI",
                if target == "aarch64-linux-android" {
                    "arm64-v8a"
                } else {
                    "x86_64"
                },
            )
            .define("ANDROID_PLATFORM", "android-26");
    }
    let artifacts = build.build();
    println!(
        "cargo:rustc-link-search=native={}",
        artifacts.join("lib").display()
    );
    for lib in [
        "dmsg_slipstream",
        "picoquic-core",
        "picotls-openssl",
        "picotls-minicrypto",
        "picotls-core",
    ] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    ssl.print_cargo_metadata();
    println!("cargo:rustc-link-lib=m");
    if !target.ends_with("android") {
        println!("cargo:rustc-link-lib=pthread");
    }
}
