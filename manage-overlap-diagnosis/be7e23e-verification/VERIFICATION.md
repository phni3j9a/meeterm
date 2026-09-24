# be7e23e verification — Manage panel overlap saga

Built fresh on `be7e23e0e9df5320ae0b601ba7ad96af7b019521` (clean CNG → pod
install → storage strip → build-for-testing, `EXPO_PUBLIC_MEETERM_SMOKE=1`,
`ENABLE_TESTABILITY=YES`, `ARCHS=arm64`; products archive sha256
`db7f772bcd549482e489d2a78bed49ba63bd2224b2456c3c000377c5f8ef29af`, manifest
file sha256 `4624a9102f9538200c9124122e6e7b23935d63f03c0a28fdef4988005fe59d05`).

## 1. First-entry LIST overlap — FIXED (verified, persists on be7e23e)

- `ac9-manage-servers-sheet.png` (the drive's own first-entry capture) and the
  simctl stills `entry-03..05` (~1–3 s in): "Saved servers" heading → full
  2-line intro → "Your servers, saved on this device." → Add server → rows —
  all separated. The structural fix (title row inside ListHeaderComponent,
  FlatList as the SafeAreaView's only child) holds.
- Interactive seeded-route check (`meeterm://smoke?screen=session-switcher-current`
  + GUI tap on "Manage servers"): clean first entry, no overlap.
- Same result on 461d2a7 earlier (first-entry list clean there too).

## 2. NEW DEFECT — embedded Add/Edit form is overlapped AND its Back is dead

FAILED VERIFICATION on a different surface, reproduced on **two consecutive
builds** (461d2a7 and be7e23e, identical stage signature) and interactively.

Observable evidence:

- XCTest `testAc9SwitcherDiagnostics` FAILED identically on both builds:
  `ac9_manage_edit_form=yes` is the last recorded stage; after the Edit form's
  "Back to saved servers" tap, `Saved servers` heading / `Server options` /
  `Connect saved server Fixture One` are all absent for their full timeouts
  (8–10 s each) → `XCTAssertTrue failed - Fixture One row missing in Manage.`
- `ac9-manage-edit-form.png` (capture ~1 s after the Edit form opened): the
  form's header ("Back"/"Edit server"/"Save") is painted at the sheet's TOP
  EDGE overlapping the grabber zone, and the scroll content ("Save a server on
  this device…") is shifted up ~90 px, colliding with the heading — the same
  overlap signature as the list defect, now on the form's own ScrollView.
- XCTest screen recording (`xctest-*.png`): the Add-server form was CLEAN at
  first paint (`m016`) then the same ~90 px upward shift appeared ~4 s later
  (`m020`); the Edit form stayed overlapped through the failure window
  (`f030` = final frame, still overlapped).
- Interactive reproduction on be7e23e (seeded switcher → Manage → Add server):
  the form opens overlapped (`interactive-add-form-overlap-8s.png`, still
  overlapped at 8 s — settled, not transient). Three GUI taps directly on the
  painted "Back" text did NOT close the form — the button's painted position
  falls inside the sheet-grabber dead zone, so touches never reach it. The
  only way out is dismissing the whole sheet.
- On `ba707cd` (before the ListHeaderComponent restructure) the identical test
  steps passed end-to-end (edit_form → Back → options → remove → retarget →
  restored, all `yes`), and the preserved ba707cd add-form stills are clean —
  this is a REGRESSION introduced by the 461d2a7 restructure, persisting into
  be7e23e.

Mechanism note for the fix author: `SwitcherManage` renders
`<ConnectionForm embedded>` as the whole panel when `form.visible` — the
form's ScrollView is nested inside ConnectionForm's own header+ScrollView
tree, so it is again NOT a direct child of the formSheet's content wrapper
and hits the same `RNSScreenContentWrapper` (0,0) class defect. The delayed
clean→overlap transition on the Add form suggests the wrong frame can also be
applied at a re-layout, not only at first mount.

## 3. Suite state

`ac9_manage_sheet=yes`, `ac9_switcher_after_manage=presented`,
`ac9_manage_profile_row=yes`, `ac9_discard_prompt=yes`,
`ac9_keep_editing=yes`, `ac9_dirty_swipe_gated=yes`,
`ac9_discard_return=yes`, `ac9_manage_save_row=yes`,
`ac9_manage_options=yes`, `ac9_manage_edit_form=yes` — everything up to the
Edit→Back step is green; the defect blocks steps 5b–7 (remove-confirm,
retarget, restored) from executing.

Evidence in this folder: VERIFICATION.md, XCTest's own captures, entry stills
(first-entry list), video frames (clean→shift transition + final state),
interactive GUI captures, stage log, failure/timing logs, raw xcodebuild log.

Artifacts NOT captured this run: simctl recordVideo (XCTest's own screen
recording holds the host recorder slot — its mp4 is the source of the
`xctest-*.png` frames above, exported from the xcresult).
