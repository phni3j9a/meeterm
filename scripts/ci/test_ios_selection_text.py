"""Exercise the production Swift clipboard reader with deterministic FFI races."""

import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SWIFTC = os.environ.get("MEETERM_SWIFTC") or shutil.which("swiftc")


@unittest.skipUnless(SWIFTC, "Swift compiler required; mandatory in macOS preflight")
class SelectionTextTests(unittest.TestCase):
    def test_copy_handles_pane_deletion_and_selection_changes(self):
        source = (ROOT / "modules/meeterm-terminal/ios/MeetermCore.swift").read_text()
        start = source.index("  static func selectionText(terminalId:")
        end = source.index("  @discardableResult static func setTheme", start)
        # Compile the actual production method, not a second implementation.
        reader = source[start:end]
        harness = r'''
import Foundation

var replies: [Int] = []
var calls = 0
func meeterm_selection_text(_ id: UInt64, _ output: UnsafeMutablePointer<UInt8>?, _ capacity: Int) -> Int {
  precondition(!replies.isEmpty)
  calls += 1
  let result = replies.removeFirst()
  if let output, result > 0 && result <= capacity {
    for index in 0..<result { output[index] = 65 }
  }
  return result
}

func check(_ responses: [Int], _ expected: String?, _ expectedCalls: Int) {
  replies = responses
  calls = 0
  let actual = MeetermCore.selectionText(terminalId: 1)
  precondition(actual == expected, "Unexpected clipboard result")
  precondition(calls == expectedCalls, "Unexpected retry count")
}

check([4, -1], nil, 2) // Pane deleted after a successful size query.
check([4, 4 * 1024 * 1024 + 1], nil, 2) // Selection exceeds native limit.
check([4, 0], "", 2) // Selection cleared before the copy.
check([4, 2], "AA", 2) // Selection shrinks.
check([2, 4, 4], "AAAA", 3) // Selection grows; retry with new capacity.
check([1, 2, 3, 4], nil, 4) // Repeated growth stays bounded.
check([-1], nil, 1) // Already deleted before the initial query.
check([0], nil, 1) // No selected text.
print("Swift clipboard reader: 8 fault-injection cases passed")
'''
        # Top-level executable statements follow the compiled production enum.
        program = "enum MeetermCore {\n" + reader + "}\n" + harness
        with tempfile.TemporaryDirectory(prefix="meeterm-selection-test-") as directory:
            path = Path(directory)
            (path / "main.swift").write_text(program)
            target_flags = []
            if platform.system() == "Darwin":
                sdk = subprocess.check_output(
                    ["xcrun", "--sdk", "macosx", "--show-sdk-path"],
                    text=True, timeout=10,
                ).strip()
                # This executable runs on the macOS host, not the iOS SDK
                # selected by the preceding Simulator typecheck.
                target_flags = ["-sdk", sdk, "-target",
                                f"{platform.machine()}-apple-macosx15.0"]
            build = subprocess.run(
                [SWIFTC, *target_flags, str(path / "main.swift"), "-o", str(path / "test")],
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(build.returncode, 0, build.stderr)
            result = subprocess.run(
                [str(path / "test")], capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("8 fault-injection cases passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
