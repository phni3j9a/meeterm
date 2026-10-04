package dev.meeterm.terminal

/** The attachment flow only offers photo-library routes. */
internal enum class AttachmentPickerRoute {
  PHOTO_PICKER,
  GALLERY,
}

/** Choose the dedicated Photos picker when available; otherwise launch Gallery. */
internal fun attachmentPickerRoute(
  photoPickerIntentAvailable: Boolean,
): AttachmentPickerRoute = if (photoPickerIntentAvailable) {
  AttachmentPickerRoute.PHOTO_PICKER
} else {
  AttachmentPickerRoute.GALLERY
}
