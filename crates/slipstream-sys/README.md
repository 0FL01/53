# Managed Slipstream native boundary

Builds the production C client from Git revision
`d7cd5555a88933053551128ff8b3741ae93049a0`, its pinned submodules and the
upstream-pinned picotls revision. OpenSSL is built from locked `openssl-src`
3.5.4. Git archives, patches, fetched dependencies and binaries stay in Cargo's
`OUT_DIR` under `target/`. The live submodule is never patched or checked out.
`native/embedding.patch` is the only transport-source delta; its hooks are
conditional on `DMSG_EMBEDDED`, so normal CLI/server behavior is preserved.

## Rust API

- `Config::new(domain: String, resolvers: Vec<SocketAddr>, certificate: Vec<u8>)`
  defaults to `Dcubic`, active keepalive 400 ms, idle keepalive 5000 ms, and an
  ephemeral local port. Public fields also select `Bbr` and a fixed local port.
- `NativeClient::start(Config) -> Result<NativeClient, Error>` reserves the one
  process-local instance and starts a worker. Invalid Rust configuration,
  allocation and thread-spawn errors are returned directly. Native setup errors
  are asynchronous `Status::Failed(code)`.
- `status(&self) -> Status`: `Starting`, `Listening(SocketAddrV4)`,
  `Ready(SocketAddrV4)`, `Stopped`, or `Failed(i32)`.
  Failure 7 records a presented full-certificate mismatch, separately from
  unreachable DNS/bootstrap (1) or an invalid local pin (4). It cannot be
  mistaken for ordinary offline availability by core's send queue.
- `endpoint(&self) -> Option<SocketAddrV4>` exposes the bound address while
  listening/ready. `Listening` **does not imply a QUIC handshake**.
- `request_stop(&self)` is a thread-safe, nonblocking cancellation request.
- `stop(&mut self) -> Result<Status, Error>` requests cancellation, joins the
  native worker, and releases the single-instance reservation. It is idempotent.
  Drop performs the same stop/join. A terminal failed worker still owns its
  reservation until joined. A late stop preserves a recorded native failure.

All resolver addresses are numeric UDP endpoints (1–8, same address family,
nonzero ports, no IPv6 scope/flow identifiers). The C loop binds **only
127.0.0.1**, installs no signal handlers, does not consume stdin credentials,
and receives its configuration and certificate in memory. Per-run C11 atomics
are the only fields shared with Rust threads. Runtime globals and QUIC objects
remain exclusively on the worker. Polling is capped at 50 ms for cancellation,
including before handshake readiness; stop never force-kills a thread.

The expected certificate is one full PEM leaf certificate or exact DER bytes,
at most 64 KiB. Parsing feeds the existing full-leaf DER/signature verifier;
this is not an SPKI pin or a trust-store/name/expiry policy. No certificate-file,
argv or process-environment credentials are introduced. OpenSSL configuration
loading is disabled for this embedding. Established loss ends the native run;
Rust/core decides when to reconnect. Its existing idle timeout can take about
30 seconds to detect a silent lost carrier. Per-stream Noise stays core-owned;
the endpoint is the existing raw local byte-stream transport.

## Verification

Prerequisites: initialized pinned submodules, Python 3.12+, Git, CMake, a C/C++
toolchain, Make/Ninja, pkg-config and complete Perl. Meson and the OpenSSL CLI
are also required for the synthetic upstream gates. Initial picotls FetchContent
requires Git/network access; its revision is pinned upstream.

From the repository root (the extra Perl setting uses this host's existing
complete Perl; omit it on hosts with a complete system Perl):

```sh
env -i HOME="$HOME" PATH="$PATH" \
  OPENSSL_SRC_PERL="$HOME/miniconda3/envs/jnabuild/bin/perl" \
  cargo test -p slipstream-sys

env -i HOME="$HOME" PATH="$PATH" \
  OPENSSL_SRC_PERL="$HOME/miniconda3/envs/jnabuild/bin/perl" \
  python3 crates/slipstream-sys/tests/run-native-gates.py

env -i HOME="$HOME" PATH="$PATH" \
  OPENSSL_SRC_PERL="$HOME/miniconda3/envs/jnabuild/bin/perl" \
  ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/android-ndk-r28c" \
  RUSTFLAGS='-C link-arg=-Wl,-z,max-page-size=16384' \
  cargo ndk -t arm64-v8a -P 26 build -p slipstream-sys
```

Use uppercase `-P 26`; installed cargo-ndk 4.1.2 misparses attached `-P26` and
may exit zero even when Cargo rejects its forwarded arguments. Check for Cargo's
`Finished` message. Adding `--tests` to the Android build also checks final native
linking and produces arm64/API26 PIEs with 16 KiB-aligned load segments. Android
build/link verification does not imply device or recursive-DNS acceptance.

`lifecycle.rs` exercises immediate and pre-ready cancellation, exact start/setup
errors, late-stop failure preservation, 16 simultaneous starts, 64 restart/stop
cycles, bound-port release, stable FD/task counts and unchanged SIGINT/SIGTERM
handlers. Its public certificate and unread loopback UDP sink require no live
deployment. The explicit `loopback.rs` gate is ignored in ordinary Cargo runs;
the script runs it after all four pinned Meson suites. It covers PEM/DER readiness,
wrong full-leaf rejection, eight exact raw streams and terminal established loss.
Disposable native server fixtures receive only public test key/cert paths.

The native boundary and its C dependencies were also checked with Clang
ASan/LSan and UBSan (excluding function-type checks: OpenSSL uses generic
callback casts). The compiler-host target override below selects this host's
installed Clang sanitizer runtime rather than its missing `unknown-linux-gnu`
runtime. Build artifacts remain isolated in `target/slipstream-asan`:

```sh
CLANG_HOST="$(clang -dumpmachine)"
env -i HOME="$HOME" PATH="$PATH" \
  OPENSSL_SRC_PERL="$HOME/miniconda3/envs/jnabuild/bin/perl" \
  CARGO_TARGET_DIR=target/slipstream-asan CC=clang CXX=clang++ \
  CFLAGS="--target=$CLANG_HOST -fsanitize=address,undefined -fno-sanitize=function -fno-omit-frame-pointer" \
  CXXFLAGS="--target=$CLANG_HOST -fsanitize=address,undefined -fno-sanitize=function -fno-omit-frame-pointer" \
  RUSTFLAGS="-C linker=clang -C link-arg=--target=$CLANG_HOST -C link-arg=-fsanitize=address,undefined" \
  ASAN_OPTIONS='detect_leaks=1:abort_on_error=1' UBSAN_OPTIONS='halt_on_error=1' \
  cargo test -p slipstream-sys --test lifecycle -- --nocapture
```

For the sanitized ready/stream/loss gate, use the same environment, add
`DMSG_TEST_SERVER="$PWD/target/slipstream-native-gates/slipstream-server"`, and
replace the Cargo arguments with
`test -p slipstream-sys --test loopback -- --ignored --nocapture`.
