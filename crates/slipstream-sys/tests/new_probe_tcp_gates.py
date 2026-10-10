"""Pinned default/opt-in gates, isolated from slipstream-native-gates baseline."""
import hashlib
import json
import os
import pathlib
import shlex
import shutil
import subprocess

root = pathlib.Path(__file__).resolve().parents[3]
target = root / "target/probe-tcp-check"
build = root / "target/slipstream-probe-nodelay"
log = root / "target/slipstream-probe-nodelay.log"
env = {name: os.environ[name] for name in ("HOME", "PATH", "OPENSSL_SRC_PERL", "TMPDIR") if name in os.environ}
env["CARGO_TARGET_DIR"] = str(target)
meson = shutil.which("meson", path=env["PATH"])
if meson is None:
    meson = str(pathlib.Path(env["HOME"]) / ".local/bin/meson")
pin = "d7cd5555a88933053551128ff8b3741ae93049a0"
baseline = [root / "target/slipstream-native-gates" / name
            for name in ("slipstream-client", "slipstream-server")]
baseline_hashes = {path: hashlib.sha256(path.read_bytes()).hexdigest()
                   for path in baseline if path.exists()}


def run(command, *, environment=env, output=None):
    print(shlex.join(map(str, command)), flush=True)
    return subprocess.run(command, cwd=root, env=environment, stdout=output,
                          stderr=subprocess.STDOUT if output else None, check=True)


def cargo_out(enabled):
    feature = ["--features", "probe-tcp-nodelay"] if enabled else []
    run(["cargo", "test", "--offline", "--locked", "-p", "slipstream-sys", *feature,
         "--", "--nocapture"])
    result = subprocess.run(["cargo", "build", "--offline", "--locked", "-p", "slipstream-sys",
                             *feature, "--message-format=json"],
                            cwd=root, env=env, stdout=subprocess.PIPE, check=True, text=True)
    out = None
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "build-script-executed" and "slipstream-sys" in message["package_id"]:
            out = pathlib.Path(message["out_dir"])
    if out is None:
        raise RuntimeError("Cargo did not report slipstream-sys OUT_DIR")
    cache = (out / "build/CMakeCache.txt").read_text()
    assert f"DMSG_PROBE_TCP_NODELAY:BOOL={'ON' if enabled else 'OFF'}" in cache
    flags = (out / "build/CMakeFiles/dmsg_slipstream.dir/flags.make").read_text()
    assert ("-DDMSG_PROBE_TCP_NODELAY=1" in flags) == enabled
    stamp = (out / "source/.embedding-stamp").read_text()
    assert stamp.startswith(pin)
    print(f"embedded flag={enabled}; source={out / 'source'}; stamp={stamp}", flush=True)
    return out


def option_value(native_env):
    result = subprocess.check_output([meson, "introspect", "--buildoptions", str(build)],
                                     cwd=root, env=native_env, text=True)
    return next(item["value"] for item in json.loads(result) if item["name"] == "probe_tcp_nodelay")


try:
    default = cargo_out(False)
    ssl = default / "openssl-build/install"
    native_env = dict(env, PKG_CONFIG_LIBDIR=str(ssl / "lib/pkgconfig"),
                      CMAKE_PREFIX_PATH=str(ssl))
    setup = [meson, "setup"]
    if (build / "meson-private/coredata.dat").exists():
        # Meson --wipe preserves prior option values, including a previous ON.
        setup += ["--wipe", "-Dprobe_tcp_nodelay=false"]
    setup += [str(build), str(default / "source"), "--buildtype=release", "-Ddefault_library=shared"]
    with log.open("w") as output:
        run(setup, environment=native_env, output=output)
        assert option_value(native_env) is False
        run([meson, "test", "-C", str(build), "--print-errorlogs"],
            environment=native_env, output=output)
        print("Default Meson option=false; all four pinned gates passed", flush=True)
        enabled = cargo_out(True)
        assert (default / "source/.embedding-stamp").read_bytes() == (enabled / "source/.embedding-stamp").read_bytes()
        run([meson, "configure", str(build), "-Dprobe_tcp_nodelay=true"],
            environment=native_env, output=output)
        assert option_value(native_env) is True
        run([meson, "test", "-C", str(build), "--print-errorlogs"],
            environment=native_env, output=output)
    commands = json.loads((build / "compile_commands.json").read_text())
    for name in ("slipstream_client_runtime.c", "slipstream_server_runtime.c"):
        entries = [entry for entry in commands if pathlib.Path(entry["file"]).name == name]
        assert len(entries) == 1 and "-DDMSG_PROBE_TCP_NODELAY=1" in entries[0]["command"]
    run(["cargo", "test", "--offline", "--locked", "-p", "slipstream-sys", "--features",
         "probe-tcp-nodelay", "--test", "loopback", "--", "--ignored", "--nocapture"],
        environment=dict(env, DMSG_TEST_SERVER=str(build / "slipstream-server")))
    for name in ("slipstream-client", "slipstream-server"):
        path = build / name
        print(f"candidate {path}: sha256={hashlib.sha256(path.read_bytes()).hexdigest()}", flush=True)
    print(f"Opt-in Meson option=true; all four pinned gates and embedded loopback passed; log={log}")
finally:
    for path, digest in baseline_hashes.items():
        assert hashlib.sha256(path.read_bytes()).hexdigest() == digest, f"baseline changed: {path}"
