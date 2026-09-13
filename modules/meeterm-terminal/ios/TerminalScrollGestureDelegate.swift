import UIKit

/// History scroll must not consume the navigation controller's horizontal pan.
/// Selection still supports dragging in either direction away from the edge.
final class TerminalScrollGestureDelegate: NSObject, UIGestureRecognizerDelegate {
  private let selectionActive: () -> Bool

  init(selectionActive: @escaping () -> Bool) {
    self.selectionActive = selectionActive
    super.init()
  }

  func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
    guard let pan = gestureRecognizer as? UIPanGestureRecognizer else { return true }
    let originX = pan.location(in: pan.view).x - pan.translation(in: pan.view).x
    return Self.shouldBegin(
      velocity: pan.velocity(in: pan.view), originX: originX, selecting: selectionActive()
    )
  }

  static func shouldBegin(velocity: CGPoint, originX: CGFloat, selecting: Bool) -> Bool {
    let horizontal = abs(velocity.x) >= abs(velocity.y)
    // The current English interface uses the native left-edge back gesture.
    if horizontal && velocity.x > 0 && originX <= 24 { return false }
    return selecting || !horizontal
  }
}
