# Development

meeterm is an Expo app with a shared Rust terminal core and thin Android/iOS
adapters. Use [TESTING.md](TESTING.md) to choose checks; mobile diagnostics are
optional tools described in [CI_MOBILE.md](CI_MOBILE.md).

## Local work

```sh
npm ci
npm run typecheck
npm run test:app
npm start
```

Use Expo Development Builds for native development; Expo Go cannot load the
custom terminal module. `npm run android` / `npm run ios` build the respective
native app when that platform's SDK/toolchain is installed.
The standalone Android evaluation APK is available from [Releases](ANDROID_RELEASES.md).

Source locations:

- `App.tsx`, `app/`: screens, navigation, settings and low-frequency state.
- `native/meeterm-core/`: terminal state, input, SSH/backend lifecycle and registry.
- `modules/meeterm-terminal/`: typed control bridge and native adapters.
- `app.json`, `plugins/`: CNG/build configuration.
- `scripts/ci/`, `scripts/ssh/`: focused checks and explicit mobile diagnostics.

`android/` and `ios/` are ignored CNG output. Change source/config and regenerate
the affected platform; do not preserve fixes only in generated Gradle/Xcode files.
Keep dependency updates separate from unrelated fixes where possible.

## Finishing a change

Implement the requested behavior and run the relevant checks in TESTING.md.
Record the result and limitations in the PR; update the document that owns the
changed behavior. No full simulator matrix, extra evidence document, or update
to every guide is required for a small change.

For product decisions see [PRODUCT.md](PRODUCT.md) and
[ENGINEERING_PRINCIPLES.md](ENGINEERING_PRINCIPLES.md). For native/SSH changes read
the relevant section of [ARCHITECTURE.md](ARCHITECTURE.md).
Historical milestone and device results remain in [evidence/](evidence/).
