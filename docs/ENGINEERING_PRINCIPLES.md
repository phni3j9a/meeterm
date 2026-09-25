# Engineering principles

These principles guide product and architecture decisions. Follow explicit
product priorities in an approved issue when they conflict with older, stricter
implementation rules, while preserving concrete security and data-integrity
requirements.

## Product-first engineering

1. **Optimize the common path.** Prefer the main user's speed, fewest actions,
   and clear feedback over completeness for hypothetical cases.
2. **Act on concrete conflicts.** Verify the identities and capabilities the
   underlying system exposes. Do not invent a proof that its API cannot provide
   or block ordinary work only because that proof is unavailable.
3. **Ask only for a real user decision.** Before adding a confirmation or
   review step, state what meaningful choice the user can make. Do not ask users
   to judge internal uncertainty they cannot observe.
4. **Keep the state machine small.** Prefer existing state and boundaries. Add
   a phase, token, epoch, fence, retry mode, route, or abstraction only when the
   accepted behavior cannot be expressed without it.
5. **Keep test needs out of product behavior.** Make fixtures and CI work with
   test-only seams instead of complicating production UX or lifecycle.
6. **Fail closed for specific risks.** Protect host identity, credentials,
   destructive operations, and actual target conflicts. Missing theoretical
   continuity proof alone is not a reason to stop ordinary recovery.
7. **Require evidence for extra mechanisms.** A new daemon, relay, persistent
   metadata, protocol, confirmation layer, or recovery phase needs a concrete
   failure scenario or explicit product requirement. “Safer,” “more robust,”
   and “future-proof” are not evidence by themselves.
8. **Remove obsolete complexity.** Delete guards, states, tests, and documents
   made unnecessary by the current product direction. Keep compatibility only
   when a real consumer needs it.
