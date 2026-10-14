# Managed Slipstream native boundary

Builds the production C client from Git revision
`cbf7fd5e5c6e5c9c2e5dbf8892a6b3cfe5ce8cb4` (`fix/bounded-early-repair`), its pinned submodules and the
upstream-pinned picotls revision. OpenSSL is built from locked `openssl-src`
3.5.4. Git archives, patches, fetched dependencies and binaries stay in Cargo's
`OUT_DIR` under `target/`. The build never mutates the live submodule; approved
source edits are committed in the transport repository, then its gitlink/PIN
are updated together. This revision fixes prepared data/ACK packet submission
in the poll path, explicit client encoding failure, duplicate POLL ACK credit
and receive ACK metadata context ownership through tracked Picoquic overlays.
In-zone IN-class non-TXT probes return NOERROR/NODATA instead of denying the
subtree used by descendant TXT traffic, preserving QName-minimization continuation.
Successfully decoded TXT with no prepared reply also returns NOERROR/NODATA;
transport silence does not change the existence of the same valid QName.
The receiver requests normal ACK scheduling for decoded POLLs, allowing the
response slot to acknowledge reception without changing sender/PTO classification.
Speculative POLLs leave up to two MTUs of congestion credit for fresh data and
an early repair; small windows still admit one pull whose ACK releases credit.
Recovery anchors PTO to the last ACK-eliciting packet in its existing context;
new ACK/POLL-only packets cannot postpone outstanding STREAM recovery. RTO,
backoff, RACK and application bounds are unchanged. This revision also enables
the existing early-repeat mechanism for unacknowledged open-STREAM data. Its
delay leaves at least a quarter-RTT selection interval before the existing half-RTT
exclusion boundary; timing at RTTs of 400ms and above and congestion admission
are unchanged. Unknown/tiny RTTs retain the ACK-delay floor. Due copies may share remaining
ACK-only packet space after fresh data, without mislabeling failed fits or
extending forced-probe PING fallback to nonempty packets. Insufficient ACK space
retains the earliest deferred packet cursor without blocking later fitting copies.
An early-copy packet is not itself early-copied; normal loss recovery of its
STREAM bytes is unchanged.
Multipath RTT periods use the
per-path packet context only for 1-RTT, not handshake/path-validation epochs.
The TCP bridge clears optimistic producer readiness after the socket is drained,
preserving queued QUIC bytes, EOF handling and readiness for the next socket write.
The server refreshes the bound backend owner's readiness before each DNS reply,
including data that arrives while earlier slots in the batch are being answered.
Existing embedding,
SPCDNS and opt-in TCP_NODELAY overlays remain separate; embedding hooks are
conditional on `DMSG_EMBEDDED` and the TCP_NODELAY option remains default-OFF.

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
- `transport_snapshot(&self) -> Option<TransportSnapshot>` copies the optional
  worker-owned connection/path evidence (unchanged155-word ABI), at most8 paths
  and64 pending packet nodes. It does not read upstream objects on the caller.
- `select_stream(&self, Option<(u16, u64)>)` selects one accepted raw stream by
  the Rust socket's **local** loopback ephemeral port and a fresh48-bit generation.
  `None`/invalid selection clears it. The port/request are private selection data,
  never diagnostic output; do not reuse a generation across lane lifetimes.
- `stream_snapshot(&self) -> Option<StreamSnapshot>` copies a separate15-word
  schema3 atomic snapshot for that selection, with three attempts and no previous-copy
  fallback. The worker scans at most64 stream entries/tree nodes without splaying
  or mutating upstream state. `tx_submitted` is QUIC sent-offset, `rx_contiguous`
  is consumed-offset, `rx_pending` is unflushed bridge output, and `rx_flushed`
  is their checked difference. `rx_highest` is the accepted STREAM end offset
  (`fin_offset`), including zero-length ranges/FIN; an excess over contiguous
  denotes an end-offset gap, not the amount of buffered suffix data.
  `tx_ack_prefix_end` is optional sender STREAM byte coverage `[0,E)`, clamped to
  submitted bytes; FIN markers do not count as bytes. Invalid/unresolved bounded
  SACK traversal yields `None`, while zero is known empty coverage. It depends on
  the pinned empty non-FIN/end-offset ACK-range correction. Missing,
  retired/reset/inconsistent streams yield
  `None`. Submission is not ACK coverage; C flush is not application read/auth.
  Native capture time has no app-clock mapping or known publication age. The
  connection/path ABI remains155 words.
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
the script runs it after all ten pinned Meson suites, including actual-loop
prepared-packet submission, real ACK/Dcubic-credit, path-local ACK metadata and
QName-minimization DNS and real PTO-anchor regressions. It covers PEM/DER readiness,
wrong full-leaf rejection, eight exact raw streams and terminal established loss.
Two additional317/911-byte selected-lane echoes compare the real worker offsets
with application writes/reads and exercise selection switching/clearing before
those original eight parallel streams. Production-C fixtures also check private
selection, protected read-only traversal, scan/read bounds, generation/retirement,
coherent atomic publication and C-to-Rust decoding, normally and under ASan/UBSan.
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
