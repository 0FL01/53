//! Compile the actual staged C seams both ways, without a new runtime ABI.
#![cfg(target_os = "linux")]

use std::{path::PathBuf, process::Command};

#[test]
fn staged_tcp_seams_checked_readback_ownership_and_default_off() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env!("OUT_DIR"));
    let source = out.join("source");
    for client in [true, false] {
        for enabled in [false, true] {
            let binary = out.join(format!("probe-tcp-seam-{client}-{enabled}"));
            let mut compile = Command::new("cc");
            compile.env_clear().env("TMPDIR", &out);
            for key in ["HOME", "PATH"] {
                if let Some(value) = std::env::var_os(key) {
                    compile.env(key, value);
                }
            }
            compile.args([
                "-std=gnu11",
                "-O0",
                "-UNDEBUG",
                "-ffunction-sections",
                "-fdata-sections",
                "-Wl,--gc-sections",
            ]);
            for include in [
                source.join("include"),
                source.join("src"),
                source.join("subprojects/picoquic/picoquic"),
                out.join("build/_deps/picotls-src/include"),
                out.join("openssl-build/install/include"),
            ] {
                compile.arg("-I").arg(include);
            }
            if client {
                compile.arg("-DTEST_CLIENT=1");
            }
            if enabled {
                compile.arg("-DDMSG_PROBE_TCP_NODELAY=1");
            }
            compile
                .arg(package.join("native/tests/probe_tcp_seams.c"))
                .arg("-L")
                .arg(out.join("lib"))
                .arg("-L")
                .arg(out.join("openssl-build/install/lib"))
                .args([
                    "-ldmsg_slipstream",
                    "-lpicoquic-core",
                    "-lpicotls-openssl",
                    "-lpicotls-minicrypto",
                    "-lpicotls-core",
                    "-lssl",
                    "-lcrypto",
                    "-lm",
                    "-lpthread",
                    "-o",
                ])
                .arg(&binary);
            let output = compile.output().expect("host C compiler");
            assert!(
                output.status.success(),
                "compile seam client={client} enabled={enabled}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = Command::new(binary).env_clear().output().unwrap();
            assert!(
                output.status.success(),
                "seam client={client} enabled={enabled}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            eprintln!(
                "client={client} enabled={enabled}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
}
