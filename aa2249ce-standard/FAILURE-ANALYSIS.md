# Failure analysis — `testStandardSeededScreensAndFoundation`, theme_dialog dark leg

## First failing stage

`xcuitest_standard` → XCTest `testStandardSeededScreensAndFoundation` →
`theme_dialog_dark` section → `exerciseSettingsDialogs` dark leg.

Assertion: `ios/meetermTests/MeetermSmokeUITests.swift:1049`
`XCTAssertTrue(app.staticTexts["Light"].waitForExistence(timeout: 10), "The draft Appearance row did not update after selection.")`

UI stages last marker: `theme_dialog_dark_settings_open` (no `theme_dialog_dark_complete`).

## What the transcript shows (xctest-transcript.log)

- t=252.48s  Appearance ActionSheet opened second time (`button("app-theme").tap()` → `Sheet (First Match)` exists).
- t=253.53s  `"Light" Button` found inside the sheet, hittable.
- t=253.59s  Tap synthesized on `"Light" Button`.
- t=254.19s  Sheet no longer exists (a real option was tapped — backdrop taps do not dismiss an iPhone ActionSheet).
- t=254.29–264.33s  `"Light" StaticText` polled ~8×, never existed → assertion failed at line 1049.

So the Light option WAS tapped and the sheet DID dismiss — `onPick` ran. The
question left is whether `staticTexts["Light"]` can ever observe the row.

## Root cause (test-side observability, verified)

`app/DailyUse.tsx:175` — the Appearance row is:

```
<Pressable accessibilityRole="button" accessibilityLabel="Appearance" testID="app-theme">
  <Text>Appearance</Text>
  <Text>{THEME_LABELS[theme]}</Text>   // <- the value text
</Pressable>
```

RN `Pressable` defaults `accessible` to true (`node_modules/react-native/
Libraries/Components/Pressable/Pressable.js:252` → `accessible: accessible
!== false`). An accessibility-element parent's child `Text` nodes are NOT
exposed individually to the iOS accessibility tree, so `app.staticTexts[...]`
can never resolve the row's value — regardless of whether the draft applied.
The same pattern appears at lines 1092 (`Keep editing` retains draft) and
1125 (reopened row shows applied value), so those legs would fail identically.

The theme_dialog legs were added by this very commit (`aa2249ce "test:
complete iOS independent theme and dialog acceptance (#37)"`), so this is the
first execution of that code — the observability bug shipped with the
candidate.

## Product behavior verified working (manual repro, same build)

The same `meeterm.app` product was installed on a second booted Simulator
(iPhone Air, iOS 27.0) and driven by hand: Settings (`app=dark`) → Appearance
→ ActionSheet → **Light** → the row's rendered value changed to **"Light"**
while app chrome stayed dark (correct draft-vs-applied semantics).
See `manual-repro-draft-light-applied.png`.

## Notes

- The run's `.xcresult` did not finalize (Staging-only, no Info.plist) because
  xcodebuild exited on the test failure; XCTest's own failure screenshot /
  hierarchy snapshot could not be extracted from it. The raw bundle was not
  uploaded per evidence rules anyway.
- All upstream stages passed: 26/26 standard routes, 6/6 theme matrix pairs,
  settings preview, recovery rail, real OS appearance flips (light→dark→light
  on the same native handle), pinned-terminal flips, focused keyboard leg,
  dark-leg dialog open + chooser capture. Storage (incl.
  `legacy_preferences_migration`) and input (incl. `theme_refresh`) suites
  passed all cases.
