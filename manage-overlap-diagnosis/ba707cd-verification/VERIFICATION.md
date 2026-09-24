# ba707cd verification — Manage first-entry overlap

**Verdict: FAILED VERIFICATION — the `insetAdjustment="never"` fix did NOT
resolve the first-render overlap.**

Built fresh on `ba707cd788bd7fbf7b4e6f67a4594d066e752816` (clean CNG → pod
install → storage strip → build-for-testing, `EXPO_PUBLIC_MEETERM_SMOKE=1`,
`ENABLE_TESTABILITY=YES`, `ARCHS=arm64`; products archive sha256
`45b9836ec324170b636dfe4780863987293f21acd4febb5cdbcfb6c3169bd902`, manifest
file sha256 `7604e466b70459a50993cd585da05083c099dfa2323f236200299e81a86d84a8`).
The fix IS in the shipped bundle (`insetAdjustment`/`contentInsetAdjustmentBehavior`
present in main.jsbundle) — so the overlap persisted WITH the fix compiled in.

## Evidence on ba707cd

- `ac9-manage-servers-sheet-ba707cd.png` — the AC9 drive's own first-entry
  capture: "Your servers, saved on this device." (ProfileList header) renders
  inside the title row between the "Saved servers" heading and the
  "Add, edit, or remove…" intro; the intro's wrapped second line ("open.") is
  clipped behind the "Add server" button.
- `entry-02..14.png` — simctl stills at ~0.58s across the first ~8s: the same
  interleaved state from the first painted frame through the whole list view,
  until the test navigates to the Add form.
- `ac9-manage-saved-row-ba707cd.png` — post-save return: CLEAN (heading →
  2-line intro → "Your servers" → button → rows, all separated). Same pattern
  as 2e4b3e3: ONLY the first entry is broken; every return/re-mount is clean.
- `ba707cd-xxl-manage-first-entry.png` / `ba707cd-xxl-manage-12s.png` —
  seeded-route drive at `accessibility-extra-extra-extra-large`: byte-identical
  captures 12s apart — fully settled, fully overlapped at XXXL too.
- `ac9-stages-ba707cd.txt` — all functional checks green (the defect is
  paint/layout only; element checks pass).

## Comparison vs 2e4b3e3

Same defect class and magnitude (~140px upward shift of the FlatList content),
slightly different collision: on 2e4b3e3 "Your servers…" painted onto the
"Saved servers" baseline; on ba707cd it paints one line lower, inside the title
row above the intro — the intro's second line is clipped behind the Add button.

## Mechanism note for the fix author

The residual is therefore NOT (only) FlatList `contentInsetAdjustmentBehavior`:
with `never` the automatic inset path is disabled, yet content still paints
~140px above its settled position on first mount inside the presented
formSheet. The shifted content spans the FULL panel width (x≈95 — outside the
title row's middle column at x≈245), consistent with the FlatList's *frame* (or
the flex container's frame) being laid out at a wrong origin on the first pass —
e.g. measured against the sheet's mid-animation bounds — rather than a
scroll-content inset. Later mounts resolve correctly.

## Scope

Only the first Manage entry after sheet open is affected; reopen/Back/re-entry
and all form/save returns are clean. AC9's `ac9_manage_profile_row=yes` (the
corrected check queried the row's button and found it — present but overlapped).
