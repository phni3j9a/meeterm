# Android attachment key focused presentation check

- Source: `3654607` (`fix/android-attachment-key-theme`), based on `1c95e29`.
- Scope: fix the stale dark attachment-button background in light terminal theme and prevent a 24dp VectorDrawable from scaling up to the 44dp touch target.
- Environment: local Android Emulator `pimate35`, API 35 / x86_64, 1080x2400, density 420dpi, software GPU. Only emulator-5556 was installed/operated; physical phones were not changed.
- Build: fresh Expo Android CNG generation with `EXPO_PUBLIC_MEETERM_SMOKE=1`; rebuilt affected native source and Release app with existing incremental dependency caches. This is a local test APK, not a distributed release.
- Build command: `JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64 ANDROID_HOME=/usr/lib/android-sdk ANDROID_SDK_ROOT=/usr/lib/android-sdk ANDROID_NDK_HOME=/usr/lib/android-sdk/ndk/27.1.12297006 EXPO_PUBLIC_MEETERM_SMOKE=1 ./gradlew :app:assembleRelease :meeterm-terminal:testDebugUnitTest :meeterm-terminal:testReleaseUnitTest -PreactNativeArchitectures=x86_64 --no-daemon --console=plain` from `android/` (Java 17 bin first in PATH).

## Results

- Release build: passed.
- Native JUnit: Debug 80 tests, Release 80 tests, zero failures/errors.
- `npm run typecheck`: passed.
- `npm run test:app`: 124 passed.
- `git diff --check`: passed.
- Installed and launched fresh local test APK. Native-ready and first-frame markers observed; no app fatal-exception/signal marker observed during the focused check.
- Public fixture URLs: `meeterm://smoke?screen=terminal&app=light&terminal=light` and `meeterm://smoke?screen=terminal&app=dark&terminal=dark`.
- Opened the software keyboard on the terminal fixture and visually inspected `light-keyboard.png` and `dark-keyboard.png`.
- `check-key-row.py` checks the actual screenshot pixels against the accessibility bounds in matching XML captures (not source-text assertions). Both themes passed: attachment/ Esc touch targets each 115x115px (~44dp), icon ink 53x53px inside the 24dp drawable extent, centered within 2px; background exactly matches Esc (light RGB 250,248,244; dark RGB 48,44,38). See `key-row-results.json`.

## Limitations and failures investigated

- This is seeded presentation/native rendering evidence, not live SSH, Photos upload/insert, physical-device, iOS, or performance/motion proof. Existing native unit tests do not render Android widgets; the bug-specific regression here is the captured native presentation and pixel check.
- First build attempt failed because the installed Java 21 runtime has no Java compiler; used the already-installed complete Java 17 JDK instead. Initial emulator start failed because this shell lacked its already-registered kvm supplementary group; started using `sg kvm` without changing system permissions/settings or using sudo.
- An intermediate patch used an unqualified `background` inside an `apply` block, which resolved to the enclosing local Int; compilation caught it. Final patch uses explicit `attachmentButton?.background` and builds successfully.
- One initial diagnostic URL invocation was not shell-quoted, so its theme query was truncated by the Android shell. All reported light/dark captures use quoted complete URLs.
- An early screenshot immediately after keyboard appearance preceded the final composed layout; captures reported here were taken after layout settled. No frame-rate or transition-smoothness claim is made.
