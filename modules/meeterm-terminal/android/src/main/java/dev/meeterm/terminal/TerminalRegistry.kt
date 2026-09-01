package dev.meeterm.terminal

/**
 * Process-local mapping from the durable terminal identity to its Rust handle.
 *
 * A view may be recreated while React Native changes screens or surfaces. A
 * release only drops the view reference; it intentionally does not destroy
 * the Rust terminal. Destruction is reserved for an explicit test reset or a
 * future process-shutdown owner.
 */
internal object TerminalRegistry {
  private data class Entry(
    val handle: Long,
    var references: Int,
  )

  private val entries = mutableMapOf<String, Entry>()

  @Synchronized
  fun acquire(terminalId: String, columns: Int, rows: Int): Long {
    val existing = entries[terminalId]
    if (existing != null) {
      existing.references += 1
      return existing.handle
    }

    val handle = MeetermNative.create(columns, rows)
    check(handle != 0L) {
      "Rust terminal creation failed for '$terminalId' ($columns x $rows)"
    }
    entries[terminalId] = Entry(handle, references = 1)
    return handle
  }

  @Synchronized
  fun release(terminalId: String, handle: Long) {
    val entry = entries[terminalId] ?: return
    if (entry.handle != handle) return
    entry.references = (entry.references - 1).coerceAtLeast(0)
  }

  @Synchronized
  fun handleFor(terminalId: String): Long = entries[terminalId]?.handle ?: 0L

  /** Explicit test/process reset. Normal view unmount does not call this. */
  @Synchronized
  fun resetForTests() {
    entries.values.forEach { entry ->
      MeetermNative.destroy(entry.handle)
    }
    entries.clear()
  }
}
