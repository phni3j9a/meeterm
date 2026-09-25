# meeterm-terminal native module

This Expo module is the low-frequency control bridge to the shared Rust core.
Terminal bytes, cells, render frames, and IME composition stay in the native
terminal path. Native views bind to stable terminal IDs; unmounting a view does
not release the remote runtime.

## Retained recovery

When the connection has retained work, Workspaces **Reconnect** and the
recovery screen's **Retry** call `retryRecovery(terminalId, operationEpoch)`.
The native core restarts stopped or exhausted recovery with a fresh retry
budget, wakes a sleeping backoff immediately, and accepts a request during an
active attempt as a no-op. Explicit Disconnect/Change and a stale operation
epoch reject the request.

`reconnect(terminalId)` is reserved for a fresh selection flow with no retained
work, such as cold start or an explicit server/Session change. Recovery never
uses it to reopen the picker.

The first automatic attempt after transport loss starts immediately. Bounded
exponential backoff applies after failures. `setForeground(true)` and the
native no-argument `networkChanged()` notification (JNI to Rust
`ssh::network_changed()`) wake every retained recovery
currently sleeping in backoff while automatic reconnect is enabled and the app
is foregrounded. Network wake does not
interrupt a healthy connection, reset the retry budget, or start another actor;
it is a no-op in the background, when automatic reconnect is disabled, or
after explicit cancellation.

For Herdr, recovery verifies the approved SSH host/key and authentication,
compatible Herdr capability, the selected running runtime, the original stable
`terminal_id`, ordinary controller acquisition without takeover, and an
authoritative full frame. A missing Herdr server-instance continuity proof by
itself is not a stop condition. Actual target mismatch, conflict, missing
runtime/terminal, authentication failure, incompatibility, or failed required
resynchronization keeps the retained work read-only for Retry or Change.
