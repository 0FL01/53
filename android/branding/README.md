# 53 launcher

`launcher.png` is the supplied scroll/quill, cropped from 1254×1254 to
`(215, 215, 1040, 1040)` (825×825). The scroll folds and feather/shaft tips
are retained; unnecessary outer whitespace is removed. No redraw/recolour.

Legacy square/round bitmaps cover all five densities. Adaptive foreground
fits the complete artwork inside the central 66dp safe circle, preventing
launcher masks from clipping the feather. Background: `#F5F7FA`.

Regenerate/check with Pillow (from the repository root):

```sh
env -i HOME="$HOME" PATH="$PATH" python3 android/branding/build-icons.py
env -i HOME="$HOME" PATH="$PATH" python3 android/branding/build-icons.py --check
```

From `android/`, the normal SDK allowlist + `./gradlew export53Apk` builds
the main-package development APK as `../53.apk`. It keeps the existing
debug signing identity; no release signing claim or data reset. Gate exports
are rejected. APKs remain untracked build artifacts.

Verified 2026-10-01: 35 JVM tests, debug/release/test builds, APK signature and
all locale labels `53`; manual `BrandingTest` on API35 passed (1/0 skips).
Main update used `adb install -r`: UID/first-install and DB/wrapped-key bytes
unchanged. Removed only `org.dmsg.client.gate`, `.gate.test`, `.test`;
the sole remaining application is `org.dmsg.client` (`0.4.1-53`).
