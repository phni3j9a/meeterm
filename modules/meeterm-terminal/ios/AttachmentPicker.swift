import ExpoModulesCore
import PhotosUI
import UIKit
import UniformTypeIdentifiers

/**
 * System pickers for the attachment flow.
 *
 * `photos` uses `PHPickerViewController` (image filter, single selection);
 * `files` uses `UIDocumentPickerViewController` for images. Only file URLs
 * come back — `AttachmentStore` stages the bytes with its own magic checks.
 * One outstanding pick at a time per process.
 */
final class AttachmentPicker: NSObject {
  enum Source { case photos, files }

  private let store: AttachmentStore
  private var completion: (([String: Any]) -> Void)?
  private var activePicker: UIViewController?

  init(store: AttachmentStore) {
    self.store = store
  }

  func pick(
    source: Source,
    from viewController: UIViewController?,
    completion: @escaping ([String: Any]) -> Void
  ) {
    guard let presenter = viewController ?? topViewController() else {
      completion(AttachmentResults.error(
        AttachmentLimits.errorIO,
        "No foreground view controller is available for the picker."
      ))
      return
    }
    guard self.completion == nil else {
      completion(AttachmentResults.error(
        AttachmentLimits.errorState,
        "An attachment pick is already in progress."
      ))
      return
    }
    self.completion = completion

    switch source {
    case .photos:
      var configuration = PHPickerConfiguration()
      configuration.selectionLimit = 1
      configuration.filter = .images
      let picker = PHPickerViewController(configuration: configuration)
      picker.delegate = self
      activePicker = picker
      presenter.present(picker, animated: true)
    case .files:
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.image])
      picker.allowsMultipleSelection = false
      picker.delegate = self
      activePicker = picker
      presenter.present(picker, animated: true)
    }
  }

  private func topViewController() -> UIViewController? {
    UIApplication.shared.connectedScenes
      .compactMap { ($0 as? UIWindowScene)?.keyWindow }
      .first?
      .rootViewController
      .map { root -> UIViewController in
        var top = root
        while let presented = top.presentedViewController { top = presented }
        return top
      }
  }

  private func finish(_ result: [String: Any]) {
    let done = completion
    completion = nil
    activePicker = nil
    done?(result)
  }

  private func message(for errorCode: String) -> String {
    switch errorCode {
    case AttachmentLimits.errorInputTooLarge:
      return "The image is too large to attach."
    default:
      return "The image could not be copied into app storage."
    }
  }

  private func stage(_ url: URL, securityScoped: Bool) {
    let fileName = store.newStagingFileName()
    let result = store.stage(from: url, fileName: fileName, securityScoped: securityScoped)
    switch result {
    case .ok(let stagedName, let byteCount):
      finish(AttachmentResults.picked(token: stagedName, byteCount: byteCount))
    case .rejected(let errorCode):
      finish(AttachmentResults.error(errorCode, message(for: errorCode)))
    }
  }
}

extension AttachmentPicker: PHPickerViewControllerDelegate {
  func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
    picker.dismiss(animated: true)
    guard let result = results.first else {
      finish(AttachmentResults.canceled())
      return
    }
    // `loadFileRepresentation` streams the asset to a temp file — the image
    // never lands in memory as an unbounded Data before staging.
    let typeIdentifier = UTType.image.identifier
    guard result.itemProvider.hasItemConformingToTypeIdentifier(typeIdentifier) else {
      finish(AttachmentResults.error(
        AttachmentLimits.errorUnsupported,
        "The selected item is not an image."
      ))
      return
    }
    result.itemProvider.loadFileRepresentation(
      forTypeIdentifier: typeIdentifier
    ) { [weak self] url, error in
      guard let self = self else { return }
      guard let url = url, error == nil else {
        DispatchQueue.main.async {
          self.finish(AttachmentResults.error(
            AttachmentLimits.errorIO,
            "The image could not be copied into app storage."
          ))
        }
        return
      }
      // The temp URL is owned by the provider and may vanish after return —
      // stage synchronously inside the callback.
      let staged = self.store.stage(
        from: url,
        fileName: self.store.newStagingFileName(),
        securityScoped: false
      )
      DispatchQueue.main.async {
        switch staged {
        case .ok(let name, let byteCount):
          self.finish(AttachmentResults.picked(token: name, byteCount: byteCount))
        case .rejected(let errorCode):
          self.finish(AttachmentResults.error(errorCode, self.message(for: errorCode)))
        }
      }
    }
  }
}

extension AttachmentPicker: UIDocumentPickerDelegate {
  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    guard let url = urls.first else {
      finish(AttachmentResults.canceled())
      return
    }
    stage(url, securityScoped: true)
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    finish(AttachmentResults.canceled())
  }
}
