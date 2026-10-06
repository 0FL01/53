#!/usr/bin/env python3
"""Regression against pinned Git objects, never the mutable vendor working tree."""
import io
import os
import pathlib
import resource
import shutil
import subprocess
import tarfile

ROOT = pathlib.Path(__file__).resolve().parents[4]
NATIVE = pathlib.Path(__file__).resolve().parents[1]
VENDOR = ROOT / "vendor/slipstream"
PIN = "d7cd5555a88933053551128ff8b3741ae93049a0"
OUT = ROOT / ".local/edns-opt-regression"
ENV = {"HOME": os.environ["HOME"], "PATH": "/usr/local/bin:/usr/bin:/bin"}
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

def run(*args, **kwargs):
    return subprocess.run(args, env=ENV, check=True, **kwargs)

revision = run("git", "-C", str(VENDOR), "ls-tree", PIN, "extern/SPCDNS", capture_output=True).stdout.decode().split()[2]
OUT.mkdir(parents=True, exist_ok=True)
SOURCE = OUT / "source"
if SOURCE.exists():
    shutil.rmtree(SOURCE)
SOURCE.mkdir()
archive = run("git", "-C", str(VENDOR / "extern/SPCDNS"), "archive", revision, capture_output=True).stdout
with tarfile.open(fileobj=io.BytesIO(archive)) as files:
    files.extractall(SOURCE, filter="data")

def build(name, sanitize):
    binary = OUT / name
    flags = ["-std=c99", "-g", "-O1", "-UNDEBUG"]
    if sanitize:
        # Host GCC 16 cannot link its own sanitizer runtimes; clang bundles them.
        flags += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
    compiler = "clang" if sanitize else "cc"
    run(compiler, *flags, "-I", str(SOURCE / "src"), str(NATIVE / "tests/edns_opt.c"), str(SOURCE / "src/codec.c"), "-lm", "-o", str(binary))
    return binary

baseline = subprocess.run([str(build("baseline", False)), "baseline"], env=ENV, capture_output=True)
assert baseline.returncode == -6 and b"len > 4" in baseline.stderr, "baseline assertion not reproduced"
print("Pinned baseline: zero-data option SIGABRT/assert(len > 4) reproduced", flush=True)
run("patch", "--batch", "--forward", "-d", str(SOURCE), "-p1", "-i", str(NATIVE / "spcdns-opt.patch"))
run(str(build("fixed", True)))
print("Shared overlay: ASan + UBSan PASS", flush=True)
