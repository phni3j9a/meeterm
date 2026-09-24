# Task A diagnosis — first-render Manage overlap on `2e4b3e3`

**Verdict: REAL DEFECT — settled state on first entry, not a transient timing flash.**

The ProfileList header/content is painted ~140–160px too far up, over the panel's
"Saved servers" title row, from the very first rendered frame, and it never
re-settles while that first list view is on screen.

## Observable evidence

Method: existing `2e4b3e3` products (test-without-building — no rebuild), the
archived AC9 drive test on the authoritative simulator (iPhone 18 Pro Max
`7EE2EBC1-0640-4950-AAFF-B4CDF455F377`, iOS 27.0), screen-recorded +
frame-extracted at 2fps, plus a GUI-driven run on the seeded smoke route
(`meeterm://smoke?screen=session-switcher-current`) at
`accessibility-extra-extra-extra-large` Dynamic Type.

| Moment | Evidence | State |
|---|---|---|
| Switcher list before Manage tap | `frames/switcher-list-before-manage.png` | clean |
| First Manage entry, first paint (~t2s) | `frames/manage-first-paint-overlap.png`, `frames/f0002-f0006.png` | **overlapped** |
| Same view ~t3.5s, still pre-Add | `frames/f0006.png`; XCTest stills entry-02..08 (~0.5–4s, 0.35s granularity) | **still overlapped** |
| Add-server form (~t3.5s+) | `frames/f0007-f0009.png` | clean (form takes whole panel) |
| Return after Discard | `frames/f0038-f0042`, `manage-return-post-discard-clean.png` | **clean** |
| Return after Save | `frames/f0058-f0063`, `manage-return-post-save-clean.png` | **clean** — matches `ac9-manage-saved-row.png` |
| Re-entry after server re-target | `frames/f0105-f0115`, `manage-reentry-clean.png` | **clean** |
| XXXL first paint (seeded route) | `xxl-manage-first-paint.png` | **overlapped, worse** — multiple text layers collide |
| XXXL ~40s later, untouched | `xxl-manage-settled-40s.png` | **still fully overlapped — never settles** |

Independently reproduced on: the acceptance-run capture `ac9-manage-servers-sheet.png`
(Director's finding), a fresh XCTest-driven entry (this run), and the seeded
smoke route at XXXL — 3/3 first entries overlapped, 0 settled while displayed.

## Mechanism (code-level)

`app/SwitcherManage.tsx` renders `SafeAreaView(edges=[top,bottom,left,right])` →
`titleRow` → `View(flex)` → `ProfileList` (`app/DailyUse.tsx`), whose `FlatList`
uses `contentInsetAdjustmentBehavior="automatic"`. On the FIRST mount inside the
already-presented formSheet, the FlatList's automatic adjusted-inset pass computes
against an unsettled safe-area/ancestor chain and shifts the entire scroll content
upward (~the titleRow+inset delta). The FlatList does not re-adjust while mounted,
so the offset persists for the whole first view. Later mounts (every return and
re-entry) resolve correctly — the panel's insets are settled by then — so only the
first entry is affected.

## Scope notes

- Reopen/Back/re-entry: all clean after the first entry (frames above).
- Large text (accessibility-XXXL): same defect, more severe visually; persists
  ≥40s untouched — proves it is a settled layout state, not an animation race.
- AC9 XCTest drive is NOT runnable at accessibility-XXXL (separate constraint):
  `input()` fill helper fails `MeetermSmokeUITests.swift:2301 — The short field
  is not hittable` (connection-form field off-viewport at that text size); see
  `ac9-drive-xxl-raw.log`. XXXL evidence was therefore gathered via the seeded
  smoke route driven through the simulator GUI.
- The original ~58s screen recording was overwritten by a later watcher run
  before finalization; the 115 extracted frames (full record, 2fps) are what the
  frames/ directory preserves. All cited moments are from those exact frames.
- Element-existence checks never caught this: it is a pure layout/paint defect —
  all queried elements exist and are hittable.
