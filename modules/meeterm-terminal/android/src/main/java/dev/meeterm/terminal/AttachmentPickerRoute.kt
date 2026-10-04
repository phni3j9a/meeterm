package dev.meeterm.terminal

/** The attachment flow only offers photo-library routes. */
internal enum class AttachmentPickerRoute {
  PHOTO_PICKER,
  GALLERY,
  UNAVAILABLE,
}

/** Prefer the dedicated Photos picker, then the image gallery, never Files. */
internal fun attachmentPickerRoute(
  photoPickerIntentAvailable: Boolean,
  galleryIntentAvailable: Boolean,
): AttachmentPickerRoute = when {
  photoPickerIntentAvailable -> AttachmentPickerRoute.PHOTO_PICKER
  galleryIntentAvailable -> AttachmentPickerRoute.GALLERY
  else -> AttachmentPickerRoute.UNAVAILABLE
}
