#!/usr/bin/env python3
"""Testsound pickup attribution: three explicit Pulse sources, scalar JSON only.
Launch with env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR".
Capture is bounded to 20 s; 20 ms PCM scratch per source is wiped after analysis.
No audio files/waveform output, default/volume/route changes or latency tuning.
Optional stdin: three newline-delimited {"label":"TOKEN"} phase observations.
0.5 s HOST DELIVERY windows and labels are not sample-acquisition timestamps.
Sample positions use Pulse read indices, including holes. Software host ranges
use request/receipt MONOTONIC brackets and the nominal stream rate, without
interpolation or clock fitting; backend/source-latency/physical error is unknown:
physical_capture_time=false, acoustic_latency=null; the monitor is digital.
"""

import argparse
import ctypes as C
import json
import math
import os
import re
import select
import signal
import subprocess
import time


ROLES = ("moto_pickup", "mi_pickup", "speaker_monitor")
INVALID = 0xFFFFFFFF
S16LE = 3
READ_CB = C.CFUNCTYPE(None, C.c_void_p, C.c_size_t, C.c_void_p)
SUCCESS_CB = C.CFUNCTYPE(None, C.c_void_p, C.c_int, C.c_void_p)


class Error(Exception):
    pass


class SampleSpec(C.Structure):
    _fields_ = [("format", C.c_int), ("rate", C.c_uint32), ("channels", C.c_uint8)]


class BufferAttr(C.Structure):
    _fields_ = [(key, C.c_uint32) for key in ("maxlength", "tlength", "prebuf", "minreq", "fragsize")]


class Timeval(C.Structure):
    _fields_ = [("tv_sec", C.c_long), ("tv_usec", C.c_long)]


class TimingInfo(C.Structure):
    # Public v17.0 pulse/def.h, LP64 layout. All other handles stay opaque.
    _fields_ = [("timestamp", Timeval), ("synchronized_clocks", C.c_int),
                ("sink_usec", C.c_uint64), ("source_usec", C.c_uint64),
                ("transport_usec", C.c_uint64), ("playing", C.c_int),
                ("write_index_corrupt", C.c_int), ("write_index", C.c_int64),
                ("read_index_corrupt", C.c_int), ("read_index", C.c_int64),
                ("configured_sink_usec", C.c_uint64), ("configured_source_usec", C.c_uint64),
                ("since_underrun", C.c_int64)]


class Pulse:
    def __init__(self):
        self.lib = C.CDLL("libpulse.so.0")
        self.lib.pa_get_library_version.restype, self.lib.pa_get_library_version.argtypes = C.c_char_p, []
        if self.lib.pa_get_library_version() not in (b"17.0", b"17.0.0"):
            raise Error("libpulse_17_0_required")
        if (C.sizeof(TimingInfo) != 104 or TimingInfo.write_index.offset != 56 or TimingInfo.read_index.offset != 72 or
                C.sizeof(SampleSpec) != 12 or C.sizeof(BufferAttr) != 20):
            raise Error("unsupported_public_abi_layout")
        p, i = C.c_void_p, C.c_int
        bindings = [
            ("mainloop_new", p, ()), ("mainloop_get_api", p, (p,)),
            ("mainloop_iterate", i, (p, i, C.POINTER(i))), ("mainloop_free", None, (p,)),
            ("context_new", p, (p, C.c_char_p)), ("context_connect", i, (p, C.c_char_p, i, p)),
            ("context_get_state", i, (p,)), ("context_disconnect", None, (p,)),
            ("context_unref", None, (p,)), ("sample_spec_valid", i, (C.POINTER(SampleSpec),)),
            ("stream_new", p, (p, C.c_char_p, C.POINTER(SampleSpec), p)),
            ("stream_connect_record", i, (p, C.c_char_p, C.POINTER(BufferAttr), i)),
            ("stream_get_state", i, (p,)), ("stream_get_device_name", C.c_char_p, (p,)),
            ("stream_get_device_index", C.c_uint32, (p,)),
            ("stream_get_sample_spec", C.POINTER(SampleSpec), (p,)),
            ("stream_is_suspended", i, (p,)), ("stream_set_read_callback", None, (p, READ_CB, p)),
            ("stream_peek", i, (p, C.POINTER(p), C.POINTER(C.c_size_t))),
            ("stream_drop", i, (p,)), ("stream_cork", p, (p, i, SUCCESS_CB, p)),
            ("stream_update_timing_info", p, (p, SUCCESS_CB, p)),
            ("stream_get_timing_info", C.POINTER(TimingInfo), (p,)),
            ("stream_disconnect", i, (p,)), ("stream_unref", None, (p,)), ("operation_unref", None, (p,))]
        for name, result, args in bindings:
            fn = getattr(self.lib, "pa_" + name)
            fn.restype, fn.argtypes = result, list(args)
            setattr(self, name, fn)


def emit(event, **fields):
    data = (json.dumps({"event": event, **fields}, allow_nan=False, separators=(",", ":")) + "\n").encode()
    # One bounded atomic pipe write; the parent drains these small JSON packets.
    if len(data) > select.PIPE_BUF or not select.select([], [1], [], 0.1)[1]:
        raise Error("stdout_packet_too_large_or_not_drained")
    if os.write(1, data) != len(data):
        raise Error("short_json_write")


def metadata(names):
    if len(set(names)) != 3:
        raise Error("three_distinct_source_names_required")
    env = {key: os.environ[key] for key in ("HOME", "PATH", "XDG_RUNTIME_DIR") if key in os.environ}
    env["LC_ALL"] = "C"
    def rows(kind):
        result = subprocess.run(["pactl", "--format=json", "list", kind], env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=3, check=True)
        return json.loads(result.stdout)
    sources, sinks = rows("sources"), rows("sinks")
    selected = []
    for role, name in zip(ROLES, names):
        matches = [row for row in sources if row.get("name") == name]
        if len(matches) != 1:
            raise Error("selected_source_absent_or_ambiguous")
        row = matches[0]
        spec = re.fullmatch(r"(s16le|s32le)\s+(\d+)ch\s+(\d+)Hz", row.get("sample_specification", ""))
        if not spec or not 1 <= int(spec[2]) <= 32 or not 800 < int(spec[3]) <= INVALID:
            raise Error("selected_source_pcm_metadata_invalid")
        # Observed public pactl JSON: monitor_source contains the source NAME.
        monitors = [sink for sink in sinks if sink.get("monitor_source") == name]
        if (role == "speaker_monitor" and len(monitors) != 1) or (role != "speaker_monitor" and monitors):
            raise Error("selected_source_monitor_role_mismatch")
        selected.append({"role": role, "name": name, "index": row["index"], "native_format": spec[1],
                         "rate": int(spec[3]), "channels": int(spec[2])})
    if len({source["index"] for source in selected}) != 3:
        raise Error("duplicate_source_indices")
    return selected


class Stats:
    def __init__(self):
        self.frames = self.samples = self.squares = self.peak = self.clipping = self.holes = self.hole_frames = 0
        self.tone_power, self.tone_blocks = 0.0, 0
        self.position_bytes, self.position_known = None, True

    def locate(self, begin, length):
        # One contiguous, half-open window range; never bridge an unknown read.
        if not self.position_known:
            return
        if begin is None or (self.position_bytes is not None and self.position_bytes[1] != begin):
            self.position_bytes, self.position_known = None, False
        elif self.position_bytes is None:
            self.position_bytes = [begin, begin + length]
        else:
            self.position_bytes[1] = begin + length

    def report(self):
        square = self.squares / max(1, self.samples)
        tone = self.tone_power / max(1, self.tone_blocks)
        return {"frames": self.frames, "peak_fs": self.peak / 32768, "rms_fs": math.sqrt(square) / 32768,
                "clipping_samples": self.clipping, "hole_fragments": self.holes, "hole_frames": self.hole_frames,
                "tone_blocks_100ms": self.tone_blocks, "tone_400hz_rms_fs": math.sqrt(tone) / 32768,
                "tone_400hz_power_fraction": min(1.0, tone / square) if square else 0.0}


class Levels:
    def __init__(self, rate, channels):
        self.channels, self.block_frames = channels, rate // 10
        self.coefficient = 2 * math.cos(2 * math.pi * 400 / rate)
        self.state = (C.c_double * (channels * 2))()
        self.position = 0
        self.total, self.window = Stats(), Stats()

    def wipe_filter(self):
        C.memset(C.addressof(self.state), 0, C.sizeof(self.state))
        self.position = 0

    def consume(self, samples, count, begin):
        square = peak = clipping = 0
        for base in range(0, count, self.channels):
            for channel in range(self.channels):
                value, index = samples[base + channel], channel * 2
                square += value * value
                peak = max(peak, abs(value))
                clipping += value <= -32768 or value >= 32767
                previous = self.state[index]
                self.state[index] = value + self.coefficient * previous - self.state[index + 1]
                self.state[index + 1] = previous
            self.position += 1
            if self.position == self.block_frames:
                power = sum(max(0.0, self.state[j]**2 + self.state[j+1]**2 -
                                self.coefficient*self.state[j]*self.state[j+1])
                            for j in range(0, self.channels * 2, 2))
                power *= 2 / (self.channels * self.block_frames**2)
                for stats in (self.total, self.window):
                    stats.tone_power += power
                    stats.tone_blocks += 1
                self.wipe_filter()
        for stats in (self.total, self.window):
            stats.frames += count // self.channels
            stats.samples += count
            stats.squares += square
            stats.peak = max(stats.peak, peak)
            stats.clipping += clipping
        self.window.locate(begin, count * 2)


class Capture:
    def __init__(self, sources, duration, labels):
        self.p, self.sources, self.duration = Pulse(), sources, duration
        self.mainloop = self.context = None
        self.by_handle = {}
        self.error = self.stop = None
        self.start = self.end = None
        self.label, self.label_time, self.label_count = None, None, 0
        self.labels, self.label_buffer = labels, bytearray()
        self.read_callback = READ_CB(self.guard(self.read))
        self.timing_callback = SUCCESS_CB(self.guard(self.timing))
        self.uncork_callback = SUCCESS_CB(self.guard(self.uncorked))
        for source in sources:
            spec = SampleSpec(S16LE, source["rate"], source["channels"])
            if not self.p.sample_spec_valid(C.byref(spec)):
                raise Error("source_sample_spec_invalid")
            source.update(handle=None, timing_pending=False, timing_request_ns=None, timing=None, uncork_ack=None)
            source["levels"] = Levels(source["rate"], source["channels"])
            source["pcm"] = (C.c_int16 * (source["rate"] // 50 * source["channels"]))()

    def guard(self, function):
        def callback(*args):
            try:
                function(*args)
            except BaseException as exc:
                self.error = str(exc) if isinstance(exc, Error) else "callback_failed"
        return callback

    def pump(self):
        if self.labels and select.select([0], [], [], 0)[0]:
            part = os.read(0, 256)
            self.labels = bool(part)
            self.label_buffer.extend(part)
            while b"\n" in self.label_buffer:
                line, _, rest = self.label_buffer.partition(b"\n")
                self.label_buffer[:] = rest
                label = json.loads(line)["label"]
                if self.label_count >= 3 or not isinstance(label, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,31}", label):
                    raise Error("invalid_or_excess_phase_label")
                self.label, self.label_time = label, time.monotonic_ns()
                self.label_count += 1
                emit("phase_label", label=self.label, host_observed_monotonic_ns=self.label_time)
            if len(self.label_buffer) > 128 or (not part and self.label_buffer):
                raise Error("incomplete_or_oversized_phase_label")
        if self.p.mainloop_iterate(self.mainloop, 0, None) < 0 or self.p.context_get_state(self.context) in (5, 6):
            raise Error("pulse_context_failed")
        if self.error or self.stop:
            raise Error(self.error or self.stop)

    def wait(self, condition, deadline):
        while not condition():
            if time.monotonic_ns() >= deadline:
                raise Error("pulse_startup_timeout")
            self.pump()
            time.sleep(0.002)

    def check(self, source):
        handle = source["handle"]
        spec = self.p.stream_get_sample_spec(handle)
        if (self.p.stream_get_state(handle) != 2 or self.p.stream_get_device_name(handle) != source["name"].encode() or
                self.p.stream_get_device_index(handle) != source["index"] or not spec or
                (spec.contents.format, spec.contents.rate, spec.contents.channels) !=
                (S16LE, source["rate"], source["channels"])):
            raise Error("source_moved_or_negotiated_format_changed")

    def open(self):
        deadline = time.monotonic_ns() + 5_000_000_000
        self.mainloop = self.p.mainloop_new()
        if not self.mainloop:
            raise Error("mainloop_allocation_failed")
        self.context = self.p.context_new(self.p.mainloop_get_api(self.mainloop), b"dmsg-pickup-probe")
        if not self.context or self.p.context_connect(self.context, None, 1, None) < 0:  # NOAUTOSPAWN
            raise Error("pulse_connection_failed")
        self.wait(lambda: self.p.context_get_state(self.context) == 4, deadline)
        for source in self.sources:
            spec = SampleSpec(S16LE, source["rate"], source["channels"])
            handle = self.p.stream_new(self.context, source["role"].encode(), C.byref(spec), None)
            source["handle"] = handle
            if not handle:
                raise Error("stream_allocation_failed")
            self.by_handle[handle] = source
            self.p.stream_set_read_callback(handle, self.read_callback, None)
            maximum = source["rate"] * 2048 // 1000 * source["channels"] * 2
            attr = BufferAttr(maximum, INVALID, INVALID, INVALID, C.sizeof(source["pcm"]))
            # START_CORKED | NOT_MONOTONIC | NO_REMAP_CHANNELS | DONT_MOVE.
            # No ADJUST_LATENCY: leave the device latency alone.
            if self.p.stream_connect_record(handle, source["name"].encode(), C.byref(attr), 0x215) < 0:
                raise Error("stream_connect_failed")
        self.wait(lambda: all(self.p.stream_get_state(s["handle"]) == 2 for s in self.sources), deadline)
        for source in self.sources:
            self.check(source)  # A corked PipeWire source may still be suspended.
        self.start = time.monotonic_ns()
        self.end = self.start + round(self.duration * 1_000_000_000)
        for source in self.sources:
            operation = self.p.stream_cork(source["handle"], 0, self.uncork_callback, None)
            if not operation:
                raise Error("uncork_failed")
            self.p.operation_unref(operation)  # Library owns the pending request.
        self.wait(lambda: all(s["uncork_ack"] for s in self.sources), min(deadline, self.end))
        # Give initial timing a bounded opportunity; absent/corrupt data stays unknown.
        while any(s["timing_pending"] for s in self.sources) and time.monotonic_ns() < min(deadline, self.end):
            self.pump()
            time.sleep(0.002)
        for source in self.sources:
            source["levels"].window = Stats()
            source["levels"].wipe_filter()

    def uncorked(self, handle, success, _userdata):
        if not success:
            raise Error("uncork_failed")
        source = self.by_handle[handle]
        source["uncork_ack"] = time.monotonic_ns()
        source["timing_pending"] = True
        source["timing_request_ns"] = time.monotonic_ns()
        operation = self.p.stream_update_timing_info(handle, self.timing_callback, None)
        if not operation:
            raise Error("timing_request_failed")
        self.p.operation_unref(operation)

    def read(self, handle, _requested, _userdata):
        source = self.by_handle[handle]
        if self.start is None or self.stop or self.error or time.monotonic_ns() >= self.end:
            return
        for _ in range(32):
            address, length = C.c_void_p(), C.c_size_t()
            if self.p.stream_peek(handle, C.byref(address), C.byref(length)) < 0:
                raise Error("peek_failed")
            if not length.value:
                return  # Empty: drop is invalid. A nonempty hole MUST be dropped.
            try:
                frame_bytes, levels = source["channels"] * 2, source["levels"]
                if length.value % frame_bytes or levels.total.frames + levels.total.hole_frames + length.value // frame_bytes > source["rate"] * 22:
                    raise Error("fragment_or_frame_bound_invalid")
                info, read_index = self.p.stream_get_timing_info(handle), None
                if source["timing"] and source["timing"]["indices_bytes"] is not None and info:
                    raw = info.contents
                    if (not raw.read_index_corrupt and not raw.write_index_corrupt and
                            raw.read_index >= 0 and raw.read_index % frame_bytes == 0):
                        read_index = raw.read_index
                if not address.value:
                    for stats in (levels.total, levels.window):
                        stats.holes += 1
                        stats.hole_frames += length.value // frame_bytes
                    levels.window.locate(read_index, length.value)
                    levels.wipe_filter()
                else:
                    offset, pcm = 0, source["pcm"]
                    while offset < length.value and not self.stop and time.monotonic_ns() < self.end:
                        count = min(C.sizeof(pcm), length.value - offset)
                        C.memmove(C.addressof(pcm), address.value + offset, count)
                        try:
                            levels.consume(pcm, count // 2, read_index + offset if read_index is not None else None)
                        finally:
                            C.memset(C.addressof(pcm), 0, C.sizeof(pcm))
                        offset += count
            finally:
                if self.p.stream_drop(handle) < 0:
                    raise Error("drop_failed")
            if self.stop or time.monotonic_ns() >= self.end:
                return

    def timing(self, handle, success, _userdata):
        receipt = time.monotonic_ns()
        source = self.by_handle[handle]
        source["timing_pending"] = False
        info = self.p.stream_get_timing_info(handle)
        result = {"anchor_host_ns": [source["timing_request_ns"], receipt], "update_success": bool(success),
                  "indices_bytes": None, "source_sink_us": None}
        if success and info:
            raw = info.contents
            frame_bytes = source["channels"] * 2
            if (not raw.read_index_corrupt and not raw.write_index_corrupt and
                    raw.read_index >= 0 and raw.write_index >= 0 and
                    raw.read_index % frame_bytes == 0 and raw.write_index % frame_bytes == 0):
                result.update(indices_bytes=[raw.read_index, raw.write_index],
                              source_sink_us=[raw.source_usec, raw.sink_usec])
        source["timing"] = result

    def snapshot(self, begin, end):
        rows = []
        for source in self.sources:
            timing = dict(source["timing"]) if source["timing"] else {
                "update_success": False, "anchor_host_ns": None, "indices_bytes": None, "source_sink_us": None}
            anchor = timing["anchor_host_ns"]
            age = end - anchor[1] if anchor else None
            timing.update(age_since_receipt_ns=age, stale=not timing["update_success"] or age is None or age > 1_500_000_000)
            timing["anchor_uncertainty_ns"] = (anchor[1] - anchor[0] + 1) // 2 if anchor and timing["indices_bytes"] is not None else None
            suspended = self.p.stream_is_suspended(source["handle"])
            window = source["levels"].window.report()
            positions = source["levels"].window.position_bytes
            window.update(position_bytes=positions, software_host_ns=None)
            if positions is not None and timing["indices_bytes"] is not None and not timing["stale"] and suspended == 0:
                # At the server snapshot: U(write) + source - sink. No transport
                # correction: the server's timing work lies inside this bracket.
                write = timing["indices_bytes"][1]
                byte_rate = source["rate"] * source["channels"] * 2
                shift = (timing["source_sink_us"][1] - timing["source_sink_us"][0]) * 1000
                lower = (positions[0] - write) * 1_000_000_000 // byte_rate
                upper = -(-(positions[1] - write) * 1_000_000_000 // byte_rate)
                window["software_host_ns"] = [anchor[0] + lower + shift, anchor[1] + upper + shift]
            rows.append({"role": source["role"], "plane": "digital_monitor" if source["role"] == ROLES[2] else "acoustic_pickup",
                         "native_format": source["native_format"], "format": "s16le", "rate": source["rate"], "channels": source["channels"],
                         "uncork_ack_host_ns": source["uncork_ack"], "suspended": suspended,
                         "window": window, "total": source["levels"].total.report(), "timing": timing})
        return {"host_window_start_ns": begin, "host_window_end_ns": end, "window_basis": "host_delivery",
                "label": self.label, "label_host_observed_ns": self.label_time,
                "timing_basis": "pulse_software_nominal_rate",
                "timing_uncertainty_ns": {"backend": None, "source_latency": None, "physical": None},
                "physical_capture_time": False, "acoustic_latency": None, "sources": rows}

    def run(self):
        self.open()
        emit("ready", capture_start_host_ns=self.start, duration_seconds=self.duration,
             timing_ready=all(s["timing"] and s["timing"]["indices_bytes"] is not None for s in self.sources),
             physical_capture_time=False, acoustic_latency=None)
        begin = time.monotonic_ns()
        next_window = begin + 500_000_000
        while not self.stop and time.monotonic_ns() < self.end:
            self.pump()
            if time.monotonic_ns() >= next_window:
                end = time.monotonic_ns()
                for source in self.sources:
                    self.check(source)
                emit("snapshot", **self.snapshot(begin, end))
                for source in self.sources:
                    source["levels"].window = Stats()
                    source["levels"].wipe_filter()
                    if not source["timing_pending"]:
                        source["timing_pending"] = True
                        source["timing_request_ns"] = time.monotonic_ns()
                        operation = self.p.stream_update_timing_info(source["handle"], self.timing_callback, None)
                        if not operation:
                            raise Error("timing_request_failed")
                        self.p.operation_unref(operation)
                begin, next_window = end, end + 500_000_000
            time.sleep(0.002)
        if self.error:
            raise Error(self.error)
        if not self.stop and any(not source["levels"].total.frames for source in self.sources):
            raise Error("selected_source_delivered_no_pcm")
        return self.snapshot(begin, time.monotonic_ns())

    def close(self):
        for source in self.sources:
            if source["handle"]:
                self.p.stream_set_read_callback(source["handle"], READ_CB(), None)
                self.p.stream_disconnect(source["handle"])
                self.p.stream_unref(source["handle"])
                source["handle"] = None
            C.memset(C.addressof(source["pcm"]), 0, C.sizeof(source["pcm"]))
            source["levels"].wipe_filter()
        if self.context:
            self.p.context_disconnect(self.context)
            self.p.context_unref(self.context)
            self.context = None
        if self.mainloop:
            self.p.mainloop_free(self.mainloop)
            self.mainloop = None


def main():
    capture, outcome, status = None, {}, 0
    stopped = [None]
    def stop(signum, _frame):
        stopped[0] = "sigterm" if signum == signal.SIGTERM else "interrupted"
        if capture:
            capture.stop = stopped[0]
    handlers = {sig: signal.signal(sig, stop) for sig in (signal.SIGINT, signal.SIGTERM)}
    stdout_blocking = os.get_blocking(1)
    os.set_blocking(1, False)
    try:
        parser = argparse.ArgumentParser(add_help=False, description=__doc__)
        for name in ("moto-source", "mi-source", "monitor-source"):
            parser.add_argument("--" + name, required=True)
        parser.add_argument("--duration", type=float, default=16)
        parser.add_argument("--labels-stdin", action="store_true")
        args = parser.parse_args()
        if not math.isfinite(args.duration) or not 0.5 <= args.duration <= 20:
            raise Error("duration_must_be_0_5_to_20_seconds")
        sources = metadata((args.moto_source, args.mi_source, args.monitor_source))
        if stopped[0]:
            raise Error(stopped[0])
        capture = Capture(sources, args.duration, args.labels_stdin)
        outcome = capture.run()
    except BaseException as exc:
        status = 1
        outcome = {"code": str(exc) if isinstance(exc, Error) else "measurement_failed"}
    finally:
        if capture:
            capture.close()
        if stopped[0]:
            status = 143 if stopped[0] == "sigterm" else 130
        try:
            emit("error" if status else "final", status=stopped[0] or ("failed" if status else "completed"),
                 owned_mutable_pcm_zeroized=True, **outcome)
        except (OSError, Error):
            status = 1
        os.set_blocking(1, stdout_blocking)
        for sig, handler in handlers.items():
            signal.signal(sig, handler)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
