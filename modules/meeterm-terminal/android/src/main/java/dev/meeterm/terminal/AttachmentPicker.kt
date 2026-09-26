package dev.meeterm.terminal

import android.content.Intent
import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.Promise
import java.util.concurrent.atomic.AtomicLong

/**
 * System pickers for the attachment flow.
 *
 * `photos` goes through the photo picker (`PickVisualMedia`, image-only);
 * `files` goes through the document picker (`OpenDocument`/`GetContent`
 * restricted to the image MIME family). Either way only a Uri comes back —
 * the chosen bytes are then
 * staged by [AttachmentStore] with its own magic checks.
 */
internal class AttachmentPicker(
  private val appContext: AppContext,
  private val store: AttachmentStore,
) {
  enum class Source { PHOTOS, FILES }

  private val requestCounter = AtomicLong()

  fun pick(source: Source, promise: Promise) {
    val activity = appContext.currentActivity as? ComponentActivity
    if (activity == null) {
      promise.resolve(
        AttachmentResults.error(
          AttachmentLimits.ERROR_IO,
          "No foreground activity is available for the picker.",
        ),
      )
      return
    }
    val key = "meeterm_attachment_${requestCounter.incrementAndGet()}"
    val registry = activity.activityResultRegistry
    val contract: ActivityResultContract<Any?, Uri?> = when (source) {
      Source.PHOTOS -> PhotoPickerContract()
      Source.FILES -> OpenImageDocumentContract()
    }
    var launcher: ActivityResultLauncher<Any?>? = null
    val registered: ActivityResultLauncher<Any?> =
      registry.register<Any?, Uri?>(key, contract) { uri ->
        val active = launcher
        launcher = null
        active?.unregister()
        if (uri == null) {
          promise.resolve(AttachmentResults.canceled())
          return@register
        }
        when (val staged = store.stageFromUri(uri, store.newStagingFileName())) {
          is AttachmentStore.StageCopy.Ok -> promise.resolve(
            AttachmentResults.picked(staged.fileName, staged.byteCount),
          )
          is AttachmentStore.StageCopy.Rejected -> promise.resolve(
            AttachmentResults.error(
              staged.errorCode,
              if (staged.errorCode == AttachmentLimits.ERROR_INPUT_TOO_LARGE) {
                "The image is too large to attach."
              } else {
                "The image could not be copied into app storage."
              },
            ),
          )
        }
      }
    launcher = registered
    try {
      registered.launch(null)
    } catch (e: RuntimeException) {
      launcher = null
      registered.unregister()
      promise.resolve(
        AttachmentResults.error(
          AttachmentLimits.ERROR_IO,
          "The picker could not be opened.",
        ),
      )
    }
  }

  /** Image-only photo picker; falls back to documents where unsupported. */
  private class PhotoPickerContract : ActivityResultContract<Any?, Uri?>() {
    private val delegate = ActivityResultContracts.PickVisualMedia()
    override fun createIntent(
      context: android.content.Context,
      input: Any?,
    ): Intent = delegate.createIntent(
      context,
      PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
    )

    override fun parseResult(resultCode: Int, intent: Intent?): Uri? =
      delegate.parseResult(resultCode, intent)

    override fun getSynchronousResult(
      context: android.content.Context,
      input: Any?,
    ): SynchronousResult<Uri?>? = null
  }

  /**
   * Image document picker. Takeable persistable permission is requested where
   * the provider offers it, but staging only needs a one-shot read.
   */
  private class OpenImageDocumentContract : ActivityResultContract<Any?, Uri?>() {
    override fun createIntent(context: android.content.Context, input: Any?): Intent =
      Intent(Intent.ACTION_OPEN_DOCUMENT)
        .addCategory(Intent.CATEGORY_OPENABLE)
        .setType("image/*")
        .putExtra(Intent.EXTRA_LOCAL_ONLY, true)

    override fun parseResult(resultCode: Int, intent: Intent?): Uri? =
      if (resultCode == android.app.Activity.RESULT_OK) intent?.data else null

    override fun getSynchronousResult(
      context: android.content.Context,
      input: Any?,
    ): SynchronousResult<Uri?>? = null
  }
}
