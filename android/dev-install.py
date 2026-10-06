#!/usr/bin/env python3
"""Install the main development APK and verify startup, not merely its label.

Data/Keystore are retained unless --reset-data is explicitly requested. Never
changes registration policy, server pins, another application, or device radios.
"""
import argparse
import os
from pathlib import Path
import subprocess
import time
import xml.etree.ElementTree as ET

PACKAGE = "org.dmsg.client"
ROOT = Path(__file__).resolve().parents[1]


def localized_texts(name):
    """Read the checked-in translations; do not duplicate or guess UI copy."""
    return {node.text or "" for directory in ("values", "values-ru")
            for file in (ROOT / "android/app/src/main/res" / directory).glob("*.xml")
            for node in ET.parse(file).getroot().findall("string") if node.get("name") == name}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True, help="explicit ADB device")
    parser.add_argument("--reset-data", action="store_true",
                        help="DESTRUCTIVE: delete this development app's DB, history and Keystore identity")
    parser.add_argument("--no-build", action="store_true", help="install existing root 53.apk")
    parser.add_argument("--server-profile", type=Path,
                        help="absolute trusted public server-profile file for APK onboarding (not an invitation)")
    args = parser.parse_args()
    sdk = Path(os.environ.get("ANDROID_HOME", str(Path.home() / "Android/Sdk")))
    env = {"HOME": str(Path.home()), "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
           "ANDROID_HOME": str(sdk), "ANDROID_SDK_ROOT": str(sdk)}
    adb_path = sdk / "platform-tools/adb"

    def command(argv, *, timeout=120):
        return subprocess.run([str(x) for x in argv], env=env, check=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout).stdout

    def adb(*argv):
        return command([adb_path, "-s", args.serial, *argv])

    if not args.no_build:
        gradle = ["./gradlew", "export53Apk", "--no-daemon"]
        if args.server_profile:
            if not args.server_profile.is_absolute() or not args.server_profile.is_file():
                raise RuntimeError("server profile must be an absolute existing public file")
            gradle.append("-PserverProfileFile=" + str(args.server_profile))
        subprocess.run(gradle, cwd=ROOT / "android", env=env, check=True)
    apk = ROOT / "53.apk"
    if not apk.is_file():
        raise RuntimeError("53.apk missing; build export53Apk first")
    analyzers = sorted((sdk / "build-tools").glob("*/aapt"))
    if not analyzers:
        raise RuntimeError("SDK aapt unavailable")
    badging = command([analyzers[-1], "dump", "badging", apk]).decode()
    if not badging.startswith("package: name='" + PACKAGE + "'") or "application-label:'53'" not in badging:
        raise RuntimeError("refusing non-main/non-53 APK")
    adb("shell", "am", "force-stop", PACKAGE)
    result = adb("install", "-r", str(apk)).decode()
    if "Success" not in result:
        raise RuntimeError("main APK installation failed")
    if args.reset_data:
        if adb("shell", "pm", "clear", PACKAGE).decode().strip() != "Success":
            raise RuntimeError("explicit main development reset failed")
        print("Explicit dev reset completed: previous local account/history/Keystore identity deleted")
    packages = {line.removeprefix("package:") for line in adb("shell", "pm", "list", "packages").decode().splitlines()}
    for suffix in (".test", ".gate.test", ".gate"):
        name = PACKAGE + suffix
        if name in packages:
            if adb("uninstall", name).decode().strip() != "Success":
                raise RuntimeError("test duplicate cleanup failed")
    adb("shell", "am", "start", "-W", "-n", PACKAGE + "/.MainActivity")
    remote = "/data/local/tmp/53-dev-startup.xml"
    storage_errors = localized_texts("error_store")
    native_errors = localized_texts("error_native_unavailable") | localized_texts("core_missing")
    states = {text: state for name, state in (("title_connection", "connection"),
              ("title_account", "authentication"), ("title_dialogs", "dialogs"))
              for text in localized_texts(name)}
    try:
        # The hierarchy can contain dialog previews. Never create a world-readable dump.
        adb("shell", "umask 077; : > " + remote + "; chmod 600 " + remote)
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            adb("shell", "uiautomator", "dump", remote)
            tree = ET.fromstring(adb("shell", "cat", remote))
            nodes = [node for node in tree.iter("node") if node.get("package") == PACKAGE]
            status = next((node.get("text", "") for node in nodes if node.get("resource-id") == PACKAGE + ":id/status"), "")
            if status in storage_errors:
                raise RuntimeError("main storage startup failed: unsupported/corrupt local data; explicit --reset-data required for disposable dev data")
            if status in native_errors:
                raise RuntimeError("main native core unavailable")
            title = next((node.get("text") for node in nodes if node.get("resource-id") == PACKAGE + ":id/main_title"), None)
            if title in states:
                print("Main 53 startup verified: " + states[title])
                print("Startup is not DNS/signup acceptance; verify the requested network workflow separately")
                return
            time.sleep(0.5)
        raise RuntimeError("main launch readiness not reached; install alone is not acceptance")
    finally:
        adb("shell", "rm", "-f", remote)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError, ET.ParseError) as error:
        # Child output may contain user text; do not reflect it in diagnostics.
        raise SystemExit(str(error) if isinstance(error, RuntimeError) else "Development install/launch command failed")
