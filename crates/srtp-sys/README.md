# Bundled SRTP boundary

`dmsg-srtp-sys` statically builds Cisco **libSRTP 2.7.0**, with PIC and its
internal AES/HMAC-SHA1 backend. There is no system-library fallback, OpenSSL,
mbedTLS, NSS, GCM, application/test executable, debug logging or stdout/file
logging in this build. Upstream crypto self-tests at initialization remain on.
The source tree includes upstream tests/apps verbatim; they are not built.

## Provenance

`libsrtp-2.7.0/` is the unmodified source archive from the official release:

- Release: <https://github.com/cisco/libsrtp/releases/tag/v2.7.0>
- Tag commit: `ee1a77c9f9dc02c42bda9901038c500c5efe4cfa`
- Archive: <https://codeload.github.com/cisco/libsrtp/tar.gz/refs/tags/v2.7.0>
- License: [`libsrtp-2.7.0/LICENSE`](libsrtp-2.7.0/LICENSE) (BSD-3-Clause).

The native shim and CMake wrapper are outside that tree. The bundled public C
header owns policy layout. Both CMake and native initialization check 2.7.0.
Native symbols have hidden visibility to isolate the bundled implementation.

## API and lifecycle

```rust,ignore
use dmsg_srtp_sys::{Sender, Receiver};

let mut tx = Sender::new(&fresh_directional_key_and_salt, expected_ssrc)?;
let mut rx = Receiver::new(&fresh_directional_key_and_salt, expected_ssrc)?;
let ciphertext = tx.protect_rtp(&complete_rtp_packet)?;
let plaintext = rx.unprotect_rtp(&ciphertext)?;
let protected_feedback = tx.protect_rtcp(&whole_rtcp_compound)?;
let feedback = rx.unprotect_rtcp(&protected_feedback)?;
```

Constructors accept `&[u8; 30]` (16-byte master key, then 14-byte master salt)
and a host-order `u32` SSRC. Packet methods take `&[u8]` and return
`Result<Vec<u8>, String>`. The sole policy is `AES_CM_128_HMAC_SHA1_80`:
10-byte SRTP trailer, 14-byte SRTCP trailer (4-byte E/index + 10-byte auth),
no MKI, exact SSRC, 128-packet RTP replay window, repeated TX disallowed.
libSRTP manages sequence rollover/ROC and all replay/counter state.

Use independent material for each direction. Protect each complete RTCP
compound **once**, starting with SR or RR; subpacket envelope lengths are
validated. SDES CNAME/APP semantics belong to the caller's negotiated profile.
Lengths are bounded at 4096 plaintext bytes; exported constants include all
minima/maxima and trailer sizes. The C shim uses an aligned bounded scratch
allocation with the full upstream maximum trailer reservation, so unaligned
Rust slices and extension/padding edge cases cannot violate its C preconditions.
Authentication/replay errors are returned as static-text Strings and never
produce partially decrypted output. Ciphertext headers are structurally checked
before native processing; padding and encrypted RTCP subpackets are validated
only after authentication.

Contexts are unique owners, `Send` but not `Sync`, and have no clone, reset,
rekey, raw-handle, policy, debug or logging API. Rust `OnceLock` serializes
library initialization and handler installation; the crypto registry is then
read-only. It is intentionally process-lifetime and is never shut down while
contexts may exist. Methods require `&mut self`; `Drop` destroys native state
using libSRTP's key-erasing destructors. The shim wipes its temporary master
key copy on creation and its scratch packet allocation on every operation.
The caller retains ownership of its borrowed master key buffer and must erase
it when no longer needed. Contexts never store a Rust copy or implement Debug.

**Every replacement generation requires fresh keys/salts and SSRC.** A new
context with an old key resets counters/replay state and is not safe recovery.
Generation/key establishment is the caller's responsibility.

Build follows `opus-sys`'s CMake pattern, including Android arm64-v8a, API 26
and `ANDROID_NDK_HOME` (NDK r28+). Host native checks can be isolated with
`CARGO_TARGET_DIR=target/srtp-check` and `cargo test -p dmsg-srtp-sys`.
