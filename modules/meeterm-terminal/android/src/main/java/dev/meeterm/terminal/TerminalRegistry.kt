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
    val handle = ensure(terminalId, columns, rows)
    entries[terminalId]?.references = (entries[terminalId]?.references ?: 0) + 1
    return handle
  }

  /**
   * Return the stable handle without taking a view reference. Control-plane
   * calls can arrive before the first native view has attached, so they use
   * this method to create the Rust terminal while leaving view lifetime
   * accounting independent from the SSH connection lifetime.
   */
  @Synchronized
  fun ensure(terminalId: String, columns: Int, rows: Int): Long {
    // Pane handles already belong to the one Rust registry. Never allocate a
    // second terminal when a view switches to a remote pane.
    if (terminalId.startsWith("native:")) return nativeHandle(terminalId)
    entries[terminalId]?.let { return it.handle }

    val handle = MeetermNative.create(columns, rows)
    if (handle == 0L) return 0L
    entries[terminalId] = Entry(handle, references = 0)
    return handle
  }

  @Synchronized
  fun release(terminalId: String, handle: Long) {
    val entry = entries[terminalId] ?: return
    if (entry.handle != handle) return
    entry.references = (entry.references - 1).coerceAtLeast(0)
  }

  @Synchronized
  fun handleFor(terminalId: String): Long =
    if (terminalId.startsWith("native:")) nativeHandle(terminalId)
    else entries[terminalId]?.handle ?: 0L

  private fun nativeHandle(terminalId: String): Long {
    val handle = terminalId.removePrefix("native:").toLongOrNull()
      ?.takeIf { it > 0L } ?: return 0L
    return if (MeetermNative.terminalExists(handle)) handle else 0L
  }

  /** Explicit test/process reset. Normal view unmount does not call this. */
  @Synchronized
  fun resetForTests() {
    entries.values.forEach { entry ->
      MeetermNative.destroy(entry.handle)
    }
    entries.clear()
  }
}
