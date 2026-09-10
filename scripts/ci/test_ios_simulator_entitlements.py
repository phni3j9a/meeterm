"""Checks the disposable iOS Simulator entitlement injection boundary."""

from __future__ import annotations

import os
from pathlib import Path
import plistlib
import stat
import subprocess
import sys
import tempfile
import unittest


INJECTOR = Path(__file__).with_name("ios-inject-ui-test.py")
APP_TARGET = "A" * 24
APP_CONFIG_LIST = "B" * 24
APP_DEBUG = "C" * 24
APP_RELEASE = "D" * 24
OTHER_TARGET = "E" * 24
OTHER_CONFIG_LIST = "F" * 24
OTHER_CONFIG = "1" * 24
PROJECT_OBJECT = "2" * 24
MAIN_GROUP = "3" * 24
PRODUCTS_GROUP = "4" * 24


PROJECT_TEMPLATE = f"""// !$*UTF8*$!
{{
	archiveVersion = 1;
	classes = {{
	}};
	objectVersion = 56;
	objects = {{

/* Begin PBXBuildFile section */
/* End PBXBuildFile section */

/* Begin PBXFileReference section */
/* End PBXFileReference section */

/* Begin PBXFrameworksBuildPhase section */
/* End PBXFrameworksBuildPhase section */

/* Begin PBXGroup section */
		{MAIN_GROUP} /* Main group */ = {{
			isa = PBXGroup;
			children = (
			);
			name = Main;
			sourceTree = "<group>";
		}};
		{PRODUCTS_GROUP} /* Products */ = {{
			isa = PBXGroup;
			children = (
			);
			name = Products;
			sourceTree = "<group>";
		}};
/* End PBXGroup section */

/* Begin PBXNativeTarget section */
		{APP_TARGET} /* meeterm */ = {{
			isa = PBXNativeTarget;
			buildConfigurationList = {APP_CONFIG_LIST} /* app configs */;
			buildPhases = (
			);
			name = meeterm;
			productType = "com.apple.product-type.application";
		}};
		{OTHER_TARGET} /* other */ = {{
			isa = PBXNativeTarget;
			buildConfigurationList = {OTHER_CONFIG_LIST} /* other configs */;
			buildPhases = (
			);
			name = other;
			productType = "com.apple.product-type.framework";
		}};
/* End PBXNativeTarget section */

/* Begin PBXResourcesBuildPhase section */
/* End PBXResourcesBuildPhase section */

/* Begin PBXSourcesBuildPhase section */
/* End PBXSourcesBuildPhase section */

/* Begin PBXContainerItemProxy section */
/* End PBXContainerItemProxy section */

/* Begin PBXTargetDependency section */
/* End PBXTargetDependency section */

/* Begin XCBuildConfiguration section */
		{APP_DEBUG} /* Debug */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				OTHER_LDFLAGS = (
					"$(inherited)",
					"-ObjC",
					"-lc++",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "dev.meeterm.app";
			}};
			name = Debug;
		}};
		{APP_RELEASE} /* Release */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				OTHER_LDFLAGS = (
					"$(inherited)",
					"-ObjC",
					"-lc++",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "dev.meeterm.app";
			}};
			name = Release;
		}};
		{OTHER_CONFIG} /* Other */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				OTHER_LDFLAGS = (
					"-other",
				);
			}};
			name = Other;
		}};
/* End XCBuildConfiguration section */

/* Begin XCConfigurationList section */
		{APP_CONFIG_LIST} /* app configs */ = {{
			isa = XCConfigurationList;
			buildConfigurations = (
				{APP_DEBUG} /* Debug */,
				{APP_RELEASE} /* Release */,
			);
		}};
		{OTHER_CONFIG_LIST} /* other configs */ = {{
			isa = XCConfigurationList;
			buildConfigurations = (
				{OTHER_CONFIG} /* Other */,
			);
		}};
/* End XCConfigurationList section */

/* Begin PBXProject section */
		{PROJECT_OBJECT} /* Project object */ = {{
			isa = PBXProject;
			TargetAttributes = {{
			}};
			mainGroup = {MAIN_GROUP};
			productRefGroup = {PRODUCTS_GROUP};
			targets = (
				{APP_TARGET} /* meeterm */,
			);
		}};
/* End PBXProject section */
	}};
	rootObject = {PROJECT_OBJECT} /* Project object */;
}}
"""

SCHEME_TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<Scheme
   LastUpgradeVersion = "1600"
   version = "1.7">
   <TestAction
      buildConfiguration = "Debug">
      <Testables>
         <TestableReference
            skipped = "NO">
            <BuildableReference
               BuildableIdentifier = "primary"
               BlueprintIdentifier = "00E356ED1AD99517003FC87E"
               BuildableName = "meetermTests.xctest"
               BlueprintName = "meetermTests"
               ReferencedContainer = "container:meeterm.xcodeproj">
            </BuildableReference>
         </TestableReference>
      </Testables>
   </TestAction>
</Scheme>
"""

PODFILE_TEMPLATE = """platform :ios, '16.4'

target 'meeterm' do
  use_expo_modules!
end
"""


def make_fake_derq(directory: Path, *, mode: str = "success") -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    command = directory / "derq"
    if mode == "success":
        body = """#!/usr/bin/env python3
import pathlib
import sys
arguments = sys.argv
output = pathlib.Path(arguments[arguments.index('-o') + 1])
output.write_bytes(b'fixture-der')
"""
    elif mode == "empty":
        body = "#!/bin/sh\nexit 0\n"
    else:
        body = "#!/bin/sh\nexit 7\n"
    command.write_text(body, encoding="utf-8")
    command.chmod(command.stat().st_mode | stat.S_IXUSR)
    return command


class IOSSimulatorEntitlementInjectionTests(unittest.TestCase):
    def run_injector(self, root: Path, derq_directory: Path) -> subprocess.CompletedProcess[str]:
        ios = root / "ios"
        project_path = ios / "meeterm.xcodeproj" / "project.pbxproj"
        scheme_path = ios / "meeterm.xcodeproj" / "xcshareddata" / "xcschemes" / "meeterm.xcscheme"
        source_path = root / "fixture.swift"
        project_path.parent.mkdir(parents=True)
        scheme_path.parent.mkdir(parents=True)
        (root / "app.json").write_text(
            '{"expo":{"ios":{"bundleIdentifier":"dev.meeterm.app"}}}\n',
            encoding="utf-8",
        )
        project_path.write_text(PROJECT_TEMPLATE, encoding="utf-8")
        scheme_path.write_text(SCHEME_TEMPLATE, encoding="utf-8")
        source_path.write_text("final class Fixture {}\n", encoding="utf-8")
        (root / "ios" / "Podfile").write_text(PODFILE_TEMPLATE, encoding="utf-8")
        (root / "ClientStoreTests.swift").write_text(
            "@testable import MeetermTerminal\nfinal class ClientStoreTests {}\n",
            encoding="utf-8",
        )
        (root / "ClientStore.swift").write_text("enum ClientStore {}\n", encoding="utf-8")
        environment = os.environ.copy()
        environment["PATH"] = os.pathsep.join(
            [str(derq_directory), environment.get("PATH", "")]
        )
        return subprocess.run(
            [
                sys.executable,
                str(INJECTOR),
                str(project_path),
                str(scheme_path),
                str(source_path),
                str(root / "ClientStoreTests.swift"),
                str(root / "ClientStore.swift"),
            ],
            check=False,
            capture_output=True,
            text=True,
            cwd=root,
            env=environment,
        )

    def test_app_host_and_storage_target_preserve_existing_flags(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-entitlements-") as directory:
            root = Path(directory)
            derq_directory = root / "bin"
            make_fake_derq(derq_directory)
            result = self.run_injector(root, derq_directory)
            self.assertEqual(result.returncode, 0, result.stderr)

            project = (root / "ios" / "meeterm.xcodeproj" / "project.pbxproj").read_text()
            self.assertEqual(project.count("OTHER_LDFLAGS[sdk=iphonesimulator*]"), 2)
            self.assertIn('"OTHER_LDFLAGS[sdk=iphonesimulator*]"', project)
            self.assertEqual(project.count('"-ObjC",'), 4)
            self.assertEqual(project.count('"-lc++",'), 4)
            self.assertEqual(project.count("meeterm-simulator-entitlements.plist"), 2)
            self.assertNotIn("meetermTests-simulator-entitlements", project)
            self.assertIn("meetermStorageTests", project)
            self.assertIn("BUNDLE_LOADER = \"$(TEST_HOST)\";", project)
            self.assertIn(
                'TEST_HOST = "$(BUILT_PRODUCTS_DIR)/meeterm.app/meeterm";',
                project,
            )
            scheme = (root / "ios" / "meeterm.xcodeproj" / "xcshareddata" / "xcschemes" / "meeterm.xcscheme").read_text()
            self.assertEqual(scheme.count('parallelizable = "NO"'), 2)
            other_start = project.index(f"{OTHER_CONFIG} /* Other */")
            other_end = project.index(f"{OTHER_CONFIG_LIST} /* other configs */")
            self.assertNotIn(
                "OTHER_LDFLAGS[sdk=iphonesimulator*]",
                project[other_start:other_end],
            )
            self.assertNotIn("CODE_SIGNING_ALLOWED = YES", project)

            app_xml = root / "ios" / "meeterm" / "meeterm-simulator-entitlements.plist"
            app_der = app_xml.with_suffix(".der")
            self.assertTrue(app_der.read_bytes())
            app_entitlements = plistlib.loads(app_xml.read_bytes())
            self.assertEqual(
                app_entitlements["application-identifier"],
                "MEETERMCI.dev.meeterm.app",
            )
            self.assertEqual(
                app_entitlements["keychain-access-groups"],
                [app_entitlements["application-identifier"]],
            )
            self.assertTrue((root / "ios" / "meetermTests" / "fixture.swift").is_file())
            self.assertTrue((root / "ios" / "meetermStorageTests" / "ClientStoreTests.swift").is_file())
            self.assertFalse((root / "ios" / "meetermTests" / "ClientStoreTests.swift").exists())
            self.assertFalse((root / "ios" / "meetermStorageTests" / "ClientStore.swift").exists())
            podfile = (root / "ios" / "Podfile").read_text()
            self.assertEqual(podfile.count("target 'meetermStorageTests' do"), 1)
            self.assertIn("inherit! :search_paths", podfile)

    def test_derq_failure_is_fail_closed_before_project_write(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-entitlements-") as directory:
            root = Path(directory)
            derq_directory = root / "bin"
            make_fake_derq(derq_directory, mode="empty")
            project_path = root / "ios" / "meeterm.xcodeproj" / "project.pbxproj"
            result = self.run_injector(root, derq_directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("DER conversion failed", result.stderr)
            self.assertEqual(project_path.read_text(), PROJECT_TEMPLATE)
            self.assertNotIn("OTHER_LDFLAGS[sdk=iphonesimulator*]", project_path.read_text())
            self.assertFalse(
                (root / "ios" / "meeterm" / "meeterm-simulator-entitlements.der").exists()
            )
            self.assertEqual((root / "ios" / "Podfile").read_text(), PODFILE_TEMPLATE)

    def test_derq_nonzero_is_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-entitlements-") as directory:
            root = Path(directory)
            derq_directory = root / "bin"
            make_fake_derq(derq_directory, mode="failure")
            result = self.run_injector(root, derq_directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("DER conversion failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
