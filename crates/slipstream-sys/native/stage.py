"""Export the pinned Git objects, never edit/check out the live submodules."""
import hashlib
import io
import pathlib
import shutil
import subprocess
import sys
import tarfile

PIN = "d7cd5555a88933053551128ff8b3741ae93049a0"
vendor, dest, patch = map(pathlib.Path, sys.argv[1:])
opt_patch = pathlib.Path(__file__).with_name("spcdns-opt.patch")

def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])

if git(vendor, "rev-parse", "HEAD").decode().strip() != PIN:
    sys.exit("vendor/slipstream must be at the approved pinned revision")
stamp = PIN + hashlib.sha256(patch.read_bytes() + opt_patch.read_bytes() + pathlib.Path(__file__).read_bytes()).hexdigest()
if (dest / ".embedding-stamp").exists() and (dest / ".embedding-stamp").read_text() == stamp:
    sys.exit(0)
if dest.exists():
    shutil.rmtree(dest)
dest.mkdir(parents=True)

def export(repo, revision, out):
    out.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(git(repo, "archive", revision))) as archive:
        archive.extractall(out, filter="data")
    # Isolate git apply from the outer worktree (otherwise it can silently skip
    # patches whose paths are outside its current-directory prefix).
    subprocess.run(["git", "init", "--quiet", str(out)], check=True)
    for line in git(repo, "ls-tree", "-r", revision).decode().splitlines():
        meta, path = line.split("\t", 1)
        mode, kind, commit = meta.split()
        if mode == "160000":
            export(repo / path, commit, out / path)

export(vendor, PIN, dest)
subprocess.run(["git", "-C", str(dest / "extern/SPCDNS"), "apply", str(opt_patch.resolve())], check=True)
subprocess.run(["python3", str(dest / "patches/apply-picoquic-patch.py")], check=True)
subprocess.run(["git", "-C", str(dest), "apply", "--recount", "--unidiff-zero", str(patch.resolve())], check=True)
(dest / ".embedding-stamp").write_text(stamp)
