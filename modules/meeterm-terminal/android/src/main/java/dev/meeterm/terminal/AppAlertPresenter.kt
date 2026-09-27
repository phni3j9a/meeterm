package dev.meeterm.terminal

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.Context
import android.content.DialogInterface
import android.content.res.Configuration
import android.util.Log
import android.view.ContextThemeWrapper
import expo.modules.kotlin.Promise
import java.util.concurrent.atomic.AtomicBoolean

/**
 * App-scoped alert presenter (Issue #37 AC36 / DIALOG-001). `system`
 * inherits the platform configuration; a fixed appearance is applied through
 * a ContextThemeWrapper configuration override, so only this dialog's
 * context/subtree repaints — the Activity, window, AppCompat delegate, and
 * the real OS scheme are never touched. There is no registry, queue, or
 * persisted state; each call owns one dialog. Supplied title, message, and
 * button text are never logged or re-emitted.
 */
internal object AppAlertPresenter {
  private const val TAG = "MeetermTerminalDialog"
  private const val MAX_BUTTONS = 3

  internal enum class Appearance { SYSTEM, LIGHT, DARK }

  internal enum class ButtonStyle { DEFAULT, CANCEL, DESTRUCTIVE }

  internal data class ButtonSpec(val text: String, val style: ButtonStyle)

  internal data class Options(
    val appearance: Appearance,
    val title: String,
    val message: String?,
    val buttons: List<ButtonSpec>,
    val cancelable: Boolean,
  )

  /**
   * Resolves the caller exactly once. A button index reported before the
   * dialog's dismiss callback wins, so a dismissal can never turn an
   * approved index back into a cancellation — or vice versa.
   */
  internal class Completion(private val resolve: (Int?) -> Unit) {
    private val consumed = AtomicBoolean(false)

    fun complete(index: Int?) {
      if (consumed.compareAndSet(false, true)) {
        resolve(index)
      }
    }
  }

  private fun invalid(): Nothing = throw IllegalArgumentException("invalid_app_alert_options")

  internal fun parse(raw: Map<String, Any?>): Options {
    val appearance = when (raw["appearance"]) {
      "system" -> Appearance.SYSTEM
      "light" -> Appearance.LIGHT
      "dark" -> Appearance.DARK
      else -> invalid()
    }
    val title = raw["title"] as? String ?: invalid()
    if (raw.containsKey("message") && raw["message"] != null && raw["message"] !is String) {
      invalid()
    }
    val message = raw["message"] as? String
    val cancelable = raw["cancelable"] as? Boolean ?: invalid()
    val rawButtons = raw["buttons"] as? List<*> ?: invalid()
    if (rawButtons.isEmpty()) {
      invalid()
    }
    // The existing Alert contract shows at most three buttons; extras are
    // dropped exactly like the platform dialog semantics already in force.
    val buttons = rawButtons.take(MAX_BUTTONS).map { entry ->
      val map = entry as? Map<*, *> ?: invalid()
      val text = map["text"] as? String ?: invalid()
      val style = when (map["style"]) {
        null, "default" -> ButtonStyle.DEFAULT
        "cancel" -> ButtonStyle.CANCEL
        "destructive" -> ButtonStyle.DESTRUCTIVE
        else -> invalid()
      }
      ButtonSpec(text, style)
    }
    return Options(appearance, title, message, buttons, cancelable)
  }

  /**
   * Original JS index → platform slot, matching the existing Alert mapping:
   * the last button is positive, the one before it negative, and a third
   * leading button neutral. Index 0 therefore lands on neutral only when all
   * three slots are populated.
   */
  internal fun slotForJsIndex(index: Int, count: Int): Int = when (index) {
    count - 1 -> DialogInterface.BUTTON_POSITIVE
    count - 2 -> DialogInterface.BUTTON_NEGATIVE
    else -> DialogInterface.BUTTON_NEUTRAL
  }

  /** Fixed appearances override only the dialog context's night qualifier. */
  internal fun nightModeOverride(appearance: Appearance): Int? = when (appearance) {
    Appearance.SYSTEM -> null
    Appearance.LIGHT -> Configuration.UI_MODE_NIGHT_NO
    Appearance.DARK -> Configuration.UI_MODE_NIGHT_YES
  }

  fun present(raw: Map<String, Any?>, activity: Activity?, promise: Promise) {
    val options = try {
      parse(raw)
    } catch (_: IllegalArgumentException) {
      promise.reject("app_alert_invalid_options", "The app alert options are invalid.", null)
      return
    }
    if (activity == null || activity.isFinishing || activity.isDestroyed) {
      promise.resolve()
      return
    }
    val completion = Completion { index ->
      // Fixed, content-free markers let the smoke harness observe how each
      // alert settled without ever logging supplied dialog text.
      Log.i(
        TAG,
        if (index == null) {
          "MEETERM_SMOKE_DIALOG result=dismissed"
        } else {
          "MEETERM_SMOKE_DIALOG result=selected index=$index"
        },
      )
      promise.resolve(index)
    }
    val context = scopedContext(activity, options.appearance)
    try {
      val dialog = createDialog(context, options, completion)
      dialog.show()
      val resolved = when (
        context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK
      ) {
        Configuration.UI_MODE_NIGHT_YES -> "dark"
        Configuration.UI_MODE_NIGHT_NO -> "light"
        else -> "undefined"
      }
      Log.i(
        TAG,
        "MEETERM_SMOKE_DIALOG presented appearance=${options.appearance.name.lowercase()} resolved=$resolved",
      )
    } catch (_: RuntimeException) {
      // A presentation failure resolves as dismissal: never an approval index.
      completion.complete(null)
    }
  }

  private fun scopedContext(activity: Activity, appearance: Appearance): Context {
    val override = nightModeOverride(appearance) ?: return activity
    val configuration = Configuration(activity.resources.configuration)
    configuration.uiMode =
      (configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or override
    val wrapper = ContextThemeWrapper(activity, activity.applicationInfo.theme)
    wrapper.applyOverrideConfiguration(configuration)
    return wrapper
  }

  private fun createDialog(
    context: Context,
    options: Options,
    completion: Completion,
  ): Dialog {
    val builder = AlertDialog.Builder(context)
    builder.setTitle(options.title)
    options.message?.let { builder.setMessage(it) }
    builder.setCancelable(options.cancelable)
    options.buttons.forEachIndexed { index, button ->
      val listener = DialogInterface.OnClickListener { _, _ -> completion.complete(index) }
      when (slotForJsIndex(index, options.buttons.size)) {
        DialogInterface.BUTTON_POSITIVE ->
          builder.setPositiveButton(button.text, listener)
        DialogInterface.BUTTON_NEGATIVE ->
          builder.setNegativeButton(button.text, listener)
        else -> builder.setNeutralButton(button.text, listener)
      }
    }
    return builder.create().apply {
      setOnDismissListener { completion.complete(null) }
    }
  }
}
