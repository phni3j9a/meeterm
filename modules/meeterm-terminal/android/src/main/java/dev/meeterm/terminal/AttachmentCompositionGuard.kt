package dev.meeterm.terminal

import java.util.concurrent.ConcurrentHashMap

/**
 * Process-wide lookup of "does terminal X have an active composition?".
 *
 * `MeetermTerminalView` registers a provider that reads its own `InputSession`
 * / composing-span state on the main thread, so module AsyncFunctions can ask
 * without touching input internals. Providers are removed on view detach to
 * keep a dead view from answering for a recycled terminal id.
 */
internal object AttachmentCompositionGuard {
  @Volatile private var overrideProvider: ((String) -> Boolean)? = null

  private val providers = ConcurrentHashMap<String, () -> Boolean>()

  fun register(terminalId: String, provider: () -> Boolean) {
    providers[terminalId] = provider
  }

  fun unregister(terminalId: String, provider: () -> Boolean) {
    providers.remove(terminalId, provider)
  }

  fun isComposing(terminalId: String): Boolean {
    overrideProvider?.let { return it(terminalId) }
    return providers[terminalId]?.invoke() ?: false
  }

  /** Test hook: when set, it replaces all registered providers. */
  fun setOverrideForTesting(provider: ((String) -> Boolean)?) {
    overrideProvider = provider
  }
}
