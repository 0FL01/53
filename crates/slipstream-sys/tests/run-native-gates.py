"""Build and run the pinned synthetic Meson gates and the Rust/C ready gate."""
import json
import os
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parents[3]
env = {name: os.environ[name] for name in ("HOME", "PATH", "OPENSSL_SRC_PERL") if name in os.environ}
result = subprocess.run(["cargo", "build", "-p", "slipstream-sys", "--message-format=json"],
                        cwd=root, env=env, stdout=subprocess.PIPE, check=True, text=True)
out = None
for line in result.stdout.splitlines():
    message = json.loads(line)
    if message.get("reason") == "build-script-executed" and "slipstream-sys" in message["package_id"]:
        out = pathlib.Path(message["out_dir"])
if out is None:
    raise RuntimeError("Cargo did not report slipstream-sys OUT_DIR")
ssl = out / "openssl-build/install"
native_env = dict(env, PKG_CONFIG_LIBDIR=str(ssl / "lib/pkgconfig"), CMAKE_PREFIX_PATH=str(ssl))
build = root / "target/slipstream-native-gates"
log = root / "target/slipstream-native-gates.log"
setup = ["meson", "setup"]
if (build / "meson-private/coredata.dat").exists():
    setup.append("--wipe")
setup += [str(build), str(out / "source"), "--buildtype=release", "-Ddefault_library=shared"]
with log.open("w") as output:
    subprocess.run(setup, cwd=root, env=native_env, stdout=output, stderr=subprocess.STDOUT, check=True)
    subprocess.run(["meson", "test", "-C", str(build), "--print-errorlogs"], cwd=root,
                   env=native_env, stdout=output, stderr=subprocess.STDOUT, check=True)
print(f"Pinned Meson gates passed; output: {log.relative_to(root)}", flush=True)
subprocess.run(["cargo", "test", "-p", "slipstream-sys", "--test", "loopback", "--",
                "--ignored", "--nocapture"], cwd=root,
               env=dict(env, DMSG_TEST_SERVER=str(build / "slipstream-server")), check=True)
