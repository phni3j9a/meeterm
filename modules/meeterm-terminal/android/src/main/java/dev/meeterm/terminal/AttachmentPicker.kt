package dev.meeterm.terminal

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.provider.MediaStore
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.Promise
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

/**
 * System pickers for the attachment flow.
 *
 * The image-only photo picker returns a Uri; selected bytes are staged by
 * [AttachmentStore] with its own magic checks.
 */
internal class AttachmentPicker(
  private val appContext: AppContext,
  private val store: AttachmentStore,
) {
  private val requestCounter = AtomicLong()
  // Result callbacks fire on the main thread; the bounded stream copy moves
  // off it so a large image never stalls the UI.
  private val copyExecutor = Executors.newSingleThreadExecutor()

  fun pick(promise: Promise) {
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
    val contract: ActivityResultContract<Any?, Uri?> = PhotoPickerContract()
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
        copyExecutor.execute {
          when (val staged = store.stageFromUri(uri, store.newStagingFileName())) {
            is AttachmentStore.StageCopy.Ok -> promise.resolve(
              AttachmentResults.picked(staged.fileName),
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
      }
    launcher = registered
    try {
      registered.launch(null)
    } catch (e: ActivityNotFoundException) {
      launcher = null
      registered.unregister()
      promise.resolve(
        AttachmentResults.error(
          AttachmentLimits.ERROR_IO,
          "This device can't open a photo picker.",
        ),
      )
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

  /** Image-only system photo picker. */
  private class PhotoPickerContract : ActivityResultContract<Any?, Uri?>() {
    private val delegate = ActivityResultContracts.PickVisualMedia()
    override fun createIntent(context: android.content.Context, input: Any?): Intent {
      val photoPickerIntent = if (ActivityResultContracts.PickVisualMedia.isPhotoPickerAvailable(context)) {
        delegate.createIntent(
          context,
          PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
        ).takeIf(::isPhotosOnlyIntent)
          ?.takeIf { resolves(context, it) }
      } else {
        null
      }
      val galleryIntent = Intent(Intent.ACTION_PICK).setDataAndType(
        MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
        "image/*",
      )
      val route = attachmentPickerRoute(photoPickerIntentAvailable = photoPickerIntent != null)
      return when (route) {
        AttachmentPickerRoute.PHOTO_PICKER -> photoPickerIntent!!
        AttachmentPickerRoute.GALLERY -> galleryIntent
      }
    }

    override fun parseResult(resultCode: Int, intent: Intent?): Uri? =
      if (resultCode == Activity.RESULT_OK) intent?.data else null

    override fun getSynchronousResult(
      context: android.content.Context,
      input: Any?,
    ): SynchronousResult<Uri?>? = null

    private fun isPhotosOnlyIntent(intent: Intent): Boolean =
      intent.action != Intent.ACTION_OPEN_DOCUMENT && intent.action != Intent.ACTION_GET_CONTENT

    private fun resolves(context: android.content.Context, intent: Intent): Boolean =
      context.packageManager.resolveActivity(intent, 0) != null
  }
}
