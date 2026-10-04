import ExpoModulesCore
import PhotosUI
import UIKit

/**
 * System pickers for the attachment flow.
 *
 * `PHPickerViewController` offers one library image. Only file URLs come
 * back — `AttachmentStore` stages the bytes with its own magic checks.
 * One outstanding pick at a time per process.
 */
final class AttachmentPicker: NSObject {
  private let store: AttachmentStore
  private var completion: (([String: Any]) -> Void)?
  private var activePicker: UIViewController?

  init(store: AttachmentStore) {
    self.store = store
  }

  func pick(
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

    var configuration = PHPickerConfiguration()
    configuration.selectionLimit = 1
    configuration.filter = .images
    let picker = PHPickerViewController(configuration: configuration)
    picker.delegate = self
    activePicker = picker
    presenter.present(picker, animated: true)
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
    let typeIdentifier = "public.image"
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
        case .ok(let name, _):
          self.finish(AttachmentResults.picked(token: name))
        case .rejected(let errorCode):
          self.finish(AttachmentResults.error(errorCode, self.message(for: errorCode)))
        }
      }
    }
  }
}
