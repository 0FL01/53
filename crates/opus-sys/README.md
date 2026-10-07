# Bundled Opus

`opus-1.6.1/` is the unmodified official release from
<https://downloads.xiph.org/releases/opus/opus-1.6.1.tar.gz>, including the
generated inline models (`dnn/nolace_data.c`, `lace_data.c` and their headers).
Its copyright notices and `COPYING` are retained. No source/model fetch happens
during builds or at runtime, and no separate checksum registry is maintained.

Cargo builds a PIC static `libopus.a` with CMake, linked into the existing core
shared library. OSCE and hardening are enabled; shared libraries, programs,
tests, fixed point and DRED are disabled. Upstream CMake does not expose
BWE/QEXT options and enables neither. OSCE also compiles upstream's deep-PLC
support; our API never passes a missing/empty frame or requests FEC/PLC.

The ownership wrapper exposes only the fixed voice encoder and checked mono
16 kHz decoder. Complexity 7 selects NoLACE on supported SILK wideband packets;
the functional core test compares the same packets against complexity 0 after
warm-up. It is an activation check, not a codec quality benchmark.
