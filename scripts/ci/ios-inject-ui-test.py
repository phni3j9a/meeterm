#!/usr/bin/env python3
"""Inject disposable UI and app-hosted storage test targets into Expo CNG."""

from __future__ import annotations

import json
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


project_path = Path(sys.argv[1])
scheme_path = Path(sys.argv[2])
source_path = Path(sys.argv[3])
project = project_path.read_text(encoding="utf-8")


SIMULATOR_ENTITLEMENTS_PREFIX = "MEETERMCI."
NODE_PATCH_SCRIPT = r"""
const fs = require("fs");
const xcode = require("xcode");

const input = JSON.parse(fs.readFileSync(0, "utf8"));
const project = xcode.project(input.projectPath);
project.parseSync();
const objects = project.hash.project.objects;
const setting = '"OTHER_LDFLAGS[sdk=iphonesimulator*]"';

function targetConfigurationIds(targetId) {
  const target = objects.PBXNativeTarget[targetId];
  if (!target) throw new Error(`missing target ${targetId}`);
  const list = objects.XCConfigurationList[target.buildConfigurationList];
  if (!list) throw new Error(`missing configuration list for ${targetId}`);
  return list.buildConfigurations.map((configuration) => configuration.value);
}

function pbxQuote(value) {
  return `"${value.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;
}

function addFlags(configurationId, xmlPath, derPath) {
  const configuration = objects.XCBuildConfiguration[configurationId];
  if (!configuration || !configuration.buildSettings) {
    throw new Error(`missing build configuration ${configurationId}`);
  }
  const settings = configuration.buildSettings;
  if (Object.prototype.hasOwnProperty.call(settings, setting)) {
    throw new Error(`existing ${setting} in ${configurationId}`);
  }
  const existing = settings.OTHER_LDFLAGS;
  const values = Array.isArray(existing)
    ? existing.slice()
    : existing
      ? [existing]
      : ['"$(inherited)"'];
  values.push(pbxQuote(`-Wl,-sectcreate,__TEXT,__entitlements,${xmlPath}`));
  values.push(pbxQuote(`-Wl,-sectcreate,__TEXT,__ents_der,${derPath}`));
  settings[setting] = values;
}

const appConfigurations = targetConfigurationIds(input.appTargetId);
for (const configurationId of appConfigurations) {
  addFlags(configurationId, input.appXmlPath, input.appDerPath);
}
fs.writeFileSync(input.projectPath, project.writeSync());
"""


def read_app_bundle_identifier(project_path: Path) -> str:
    config_path = project_path.parent.parent.parent / "app.json"
    try:
        document = json.loads(config_path.read_text(encoding="utf-8"))
        value = document["expo"]["ios"]["bundleIdentifier"]
    except (KeyError, OSError, TypeError, ValueError) as error:
        raise SystemExit(f"iOS UI target injection could not read Expo iOS bundle identifier: {error}") from error
    if not isinstance(value, str) or not value:
        raise SystemExit("iOS UI target injection found an invalid Expo iOS bundle identifier")
    return value


def patch_podfile_with_storage_target(podfile_path: Path, app_target_name: str) -> str:
    try:
        podfile = podfile_path.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"iOS storage test target injection could not read Podfile: {error}") from error
    if re.search(r"^\s*target ['\"]meetermStorageTests['\"] do\s*$", podfile, re.MULTILINE):
        raise SystemExit("meetermStorageTests already exists in the generated Podfile")
    pattern = re.compile(
        rf"^target ['\"]{re.escape(app_target_name)}['\"] do\s*$",
        re.MULTILINE,
    )
    matches = list(pattern.finditer(podfile))
    if len(matches) != 1:
        raise SystemExit(
            f"iOS storage test target injection expected one Podfile target opener for {app_target_name}, "
            f"found {len(matches)}"
        )
    insertion = """
  target 'meetermStorageTests' do
    inherit! :search_paths
    # The app host already owns ExpoModulesProvider. Expo's target extension
    # inherits its manager even for search-path-only children; suppress only
    # this disposable target's provider to avoid a duplicate Objective-C class.
    current_target_definition.define_singleton_method(:autolinking_manager) { nil }
  end
"""
    end = matches[0].end()
    return podfile[:end] + insertion + podfile[end:]


def write_simulator_entitlements(
    directory: Path,
    target_label: str,
    bundle_identifier: str,
) -> tuple[str, str]:
    """Create simulator-only XML and DER entitlement inputs, then fail closed."""

    directory.mkdir(parents=True, exist_ok=True)
    safe_label = re.sub(r"[^A-Za-z0-9_-]", "_", target_label)
    xml_path = directory / f"{safe_label}-simulator-entitlements.plist"
    der_path = directory / f"{safe_label}-simulator-entitlements.der"
    app_identifier = f"{SIMULATOR_ENTITLEMENTS_PREFIX}{bundle_identifier}"
    try:
        with xml_path.open("wb") as stream:
            plistlib.dump(
                {
                    "application-identifier": app_identifier,
                    "keychain-access-groups": [app_identifier],
                },
                stream,
                fmt=plistlib.FMT_XML,
                sort_keys=True,
            )
    except OSError as error:
        raise SystemExit(f"iOS simulator entitlement XML could not be written: {error}") from error

    der_path.unlink(missing_ok=True)
    derq = shutil.which("derq")
    if derq is None:
        fallback = Path("/usr/bin/derq")
        if fallback.is_file():
            derq = str(fallback)
    if derq is None:
        raise SystemExit("iOS simulator entitlement DER conversion requires Xcode derq")
    try:
        result = subprocess.run(
            [
                derq,
                "query",
                "-f",
                "xml",
                "-i",
                str(xml_path),
                "-o",
                str(der_path),
                "--raw",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except OSError as error:
        raise SystemExit(f"iOS simulator entitlement DER conversion could not start: {error}") from error
    if result.returncode != 0 or not der_path.is_file() or der_path.stat().st_size == 0:
        raise SystemExit(
            "iOS simulator entitlement DER conversion failed "
            f"(exit={result.returncode})"
        )
    return str(xml_path), str(der_path)


def patch_project_with_simulator_flags(
    project_text: str,
    project_path: Path,
    app_target_id: str,
    app_target_name: str,
    app_entitlements: tuple[str, str],
) -> str:
    node = shutil.which("node")
    if node is None:
        raise SystemExit("iOS Simulator entitlement injection requires the project xcode parser")
    repository_root = Path(__file__).resolve().parents[2]
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=project_path.parent,
            prefix=f".{project_path.name}.",
            suffix=".tmp",
            delete=False,
        ) as stream:
            stream.write(project_text)
            temporary_path = Path(stream.name)
        payload = {
            "projectPath": str(temporary_path),
            "appTargetId": app_target_id,
            "appXmlPath": f"$(SRCROOT)/{app_target_name}/{Path(app_entitlements[0]).name}",
            "appDerPath": f"$(SRCROOT)/{app_target_name}/{Path(app_entitlements[1]).name}",
        }
        try:
            result = subprocess.run(
                [node, "-e", NODE_PATCH_SCRIPT],
                cwd=repository_root,
                input=json.dumps(payload),
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
        except OSError as error:
            raise SystemExit(f"iOS Simulator entitlement project update could not start: {error}") from error
        if result.returncode != 0:
            raise SystemExit(
                "iOS Simulator entitlement project update failed "
                f"(exit={result.returncode})"
            )
        return temporary_path.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"iOS Simulator entitlement project update failed: {error}") from error
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)

# Expo CNG emits a scheme whose TestAction references this target, but does
# not create the target in the generated project. Reusing that blueprint keeps
# ios/ disposable and avoids a checked-in Xcode project or a new dependency.
target_id = "00E356ED1AD99517003FC87E"
product_ref_id = "00E356EE1AD99517003FC87E"
source_ref_id = "00E356EF1AD99517003FC87E"
build_file_id = "00E356F01AD99517003FC87E"
tests_group_id = "00E356F11AD99517003FC87E"
sources_phase_id = "00E356F21AD99517003FC87E"
frameworks_phase_id = "00E356F31AD99517003FC87E"
resources_phase_id = "00E356F41AD99517003FC87E"
target_config_list_id = "00E356F51AD99517003FC87E"
debug_config_id = "00E356F61AD99517003FC87E"
release_config_id = "00E356F71AD99517003FC87E"
proxy_id = "00E356F81AD99517003FC87E"
dependency_id = "00E356F91AD99517003FC87E"

storage_target_id = "00E359ED1AD99517003FC87E"
storage_product_ref_id = "00E359EE1AD99517003FC87E"
storage_source_ref_id = "00E359EF1AD99517003FC87E"
storage_build_file_id = "00E359F01AD99517003FC87E"
storage_group_id = "00E359F11AD99517003FC87E"
storage_sources_phase_id = "00E359F21AD99517003FC87E"
storage_frameworks_phase_id = "00E359F31AD99517003FC87E"
storage_target_config_list_id = "00E359F51AD99517003FC87E"
storage_debug_config_id = "00E359F61AD99517003FC87E"
storage_release_config_id = "00E359F71AD99517003FC87E"
storage_proxy_id = "00E359F81AD99517003FC87E"
storage_dependency_id = "00E359F91AD99517003FC87E"


def required(pattern: str, label: str) -> str:
    match = re.search(pattern, project, re.MULTILINE | re.DOTALL)
    if match is None:
        raise SystemExit(f"iOS UI target injection could not find {label}")
    return match.group(1)


main_group_id = required(
    r"^\s*mainGroup = ([A-F0-9]{24});$", "the project main group"
)
products_group_id = required(
    r"^\s*productRefGroup = ([A-F0-9]{24})(?: /\*[^*]*\*/)?;",
    "the Products group",
)
project_object_id = required(
    r"^\s*([A-F0-9]{24}) /\* Project object \*/ = \{",
    "the project object",
)

app_target_id = ""
app_target_name = ""
for match in re.finditer(
    r"(?m)^\s*([A-F0-9]{24}) /\* ([^*]+) \*/ = \{\n"
    r"(?P<body>.*?)(?=^\s*\};)",
    project,
    re.MULTILINE | re.DOTALL,
):
    if (
        "isa = PBXNativeTarget;" in match.group("body")
        and 'productType = "com.apple.product-type.application";'
        in match.group("body")
    ):
        app_target_id = match.group(1)
        app_target_name = match.group(2).strip()
        break
if not app_target_id:
    raise SystemExit("iOS UI target injection could not find the application target")

app_bundle_identifier = read_app_bundle_identifier(project_path)
test_bundle_identifier = f"{app_bundle_identifier}.meetermTests"
storage_bundle_identifier = f"{app_bundle_identifier}.meetermStorageTests"

extra_sources = [Path(argument) for argument in sys.argv[4:]]
storage_test_sources = [source for source in extra_sources if source.name == "ClientStoreTests.swift"]
if len(storage_test_sources) != 1:
    raise SystemExit("iOS UI target injection requires exactly one ClientStoreTests.swift source")
storage_test_source = storage_test_sources[0]
ui_extra_sources = [
    source for source in extra_sources
    if source.name not in {"ClientStoreTests.swift", "ClientStore.swift"}
]

if re.search(
    rf"^\s*{re.escape(target_id)} /\* meetermTests \*/ = \{{",
    project,
    re.MULTILINE,
):
    raise SystemExit("meetermTests already exists in the generated iOS project")
if target_id in project:
    raise SystemExit("the reserved iOS UI test target ID is already in use")
if storage_target_id in project:
    raise SystemExit("the reserved iOS storage test target ID is already in use")

generated_tests_dir = project_path.parent.parent / "meetermTests"
generated_storage_dir = project_path.parent.parent / "meetermStorageTests"

app_entitlements = write_simulator_entitlements(
    project_path.parent.parent / app_target_name,
    app_target_name,
    app_bundle_identifier,
)


def insert_section(section: str, payload: str) -> None:
    global project
    end_marker = f"/* End {section} section */"
    position = project.find(end_marker)
    if position >= 0:
        project = project[:position] + payload + project[position:]
        return

    # A bare Expo project has no target-dependency sections until a second
    # target is added. Insert a complete section inside the objects dictionary.
    begin_marker = "/* Begin PBXBuildFile section */"
    position = project.find(begin_marker)
    if position < 0:
        raise SystemExit(f"iOS UI target injection could not find an insertion point for {section}")
    section_text = (
        f"/* Begin {section} section */{payload}"
        f"/* End {section} section */\n\n"
    )
    project = project[:position] + section_text + project[position:]


insert_section(
    "PBXBuildFile",
    f"""
		{build_file_id} /* {source_path.name} in Sources */ = {{isa = PBXBuildFile; fileRef = {source_ref_id} /* {source_path.name} */; }};
""",
)
insert_section(
    "PBXFileReference",
    f"""
		{product_ref_id} /* meetermTests.xctest */ = {{isa = PBXFileReference; explicitFileType = wrapper.cfbundle; includeInIndex = 0; path = meetermTests.xctest; sourceTree = BUILT_PRODUCTS_DIR; }};
		{source_ref_id} /* {source_path.name} */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; name = {source_path.name}; path = {source_path.name}; sourceTree = "<group>"; }};
""",
)
insert_section(
    "PBXFrameworksBuildPhase",
    f"""
		{frameworks_phase_id} /* Frameworks */ = {{
			isa = PBXFrameworksBuildPhase;
			buildActionMask = 2147483647;
			files = (
			);
			runOnlyForDeploymentPostprocessing = 0;
		}};
""",
)
insert_section(
    "PBXGroup",
    f"""
		{tests_group_id} /* meetermTests */ = {{
			isa = PBXGroup;
			children = (
				{source_ref_id} /* {source_path.name} */,
			);
			path = meetermTests;
			sourceTree = "<group>";
		}};
""",
)
insert_section(
    "PBXNativeTarget",
    f"""
		{target_id} /* meetermTests */ = {{
			isa = PBXNativeTarget;
			buildConfigurationList = {target_config_list_id} /* Build configuration list for PBXNativeTarget "meetermTests" */;
			buildPhases = (
				{sources_phase_id} /* Sources */,
				{frameworks_phase_id} /* Frameworks */,
				{resources_phase_id} /* Resources */,
			);
			buildRules = (
			);
			dependencies = (
				{dependency_id} /* PBXTargetDependency */,
			);
			name = meetermTests;
			productName = meetermTests;
			productReference = {product_ref_id} /* meetermTests.xctest */;
			productType = "com.apple.product-type.bundle.ui-testing";
        }};
""",
)
insert_section(
    "PBXBuildFile",
    f"""
		{storage_build_file_id} /* {storage_test_source.name} in Sources */ = {{isa = PBXBuildFile; fileRef = {storage_source_ref_id} /* {storage_test_source.name} */; }};
""",
)
insert_section(
    "PBXFileReference",
    f"""
		{storage_product_ref_id} /* meetermStorageTests.xctest */ = {{isa = PBXFileReference; explicitFileType = wrapper.cfbundle; includeInIndex = 0; path = meetermStorageTests.xctest; sourceTree = BUILT_PRODUCTS_DIR; }};
		{storage_source_ref_id} /* {storage_test_source.name} */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; name = {storage_test_source.name}; path = {storage_test_source.name}; sourceTree = "<group>"; }};
""",
)
insert_section(
    "PBXFrameworksBuildPhase",
    f"""
		{storage_frameworks_phase_id} /* Storage test frameworks */ = {{
			isa = PBXFrameworksBuildPhase;
			buildActionMask = 2147483647;
			files = (
			);
			runOnlyForDeploymentPostprocessing = 0;
		}};
""",
)
insert_section(
    "PBXGroup",
    f"""
		{storage_group_id} /* meetermStorageTests */ = {{
			isa = PBXGroup;
			children = (
				{storage_source_ref_id} /* {storage_test_source.name} */,
			);
			path = meetermStorageTests;
			sourceTree = "<group>";
		}};
""",
)
insert_section(
    "PBXNativeTarget",
    f"""
		{storage_target_id} /* meetermStorageTests */ = {{
			isa = PBXNativeTarget;
			buildConfigurationList = {storage_target_config_list_id} /* Build configuration list for PBXNativeTarget "meetermStorageTests" */;
			buildPhases = (
				{storage_sources_phase_id} /* Sources */,
				{storage_frameworks_phase_id} /* Frameworks */,
			);
			buildRules = (
			);
			dependencies = (
				{storage_dependency_id} /* PBXTargetDependency */,
			);
			name = meetermStorageTests;
			productName = meetermStorageTests;
			productReference = {storage_product_ref_id} /* meetermStorageTests.xctest */;
			productType = "com.apple.product-type.bundle.unit-test";
		}};
""",
)
insert_section(
    "PBXSourcesBuildPhase",
    f"""
		{storage_sources_phase_id} /* Storage test sources */ = {{
			isa = PBXSourcesBuildPhase;
			buildActionMask = 2147483647;
			files = (
				{storage_build_file_id} /* {storage_test_source.name} in Sources */,
			);
			runOnlyForDeploymentPostprocessing = 0;
		}};
""",
)
insert_section(
    "PBXContainerItemProxy",
    f"""
		{storage_proxy_id} /* Storage app proxy */ = {{
			isa = PBXContainerItemProxy;
			containerPortal = {project_object_id} /* Project object */;
			proxyType = 1;
			remoteGlobalIDString = {app_target_id};
			remoteInfo = {app_target_name};
		}};
""",
)
insert_section(
    "PBXTargetDependency",
    f"""
		{storage_dependency_id} /* Storage app dependency */ = {{isa = PBXTargetDependency; target = {app_target_id} /* {app_target_name} */; targetProxy = {storage_proxy_id} /* Storage app proxy */; }};
""",
)
insert_section(
    "PBXResourcesBuildPhase",
    f"""
		{resources_phase_id} /* Resources */ = {{
			isa = PBXResourcesBuildPhase;
			buildActionMask = 2147483647;
			files = (
			);
			runOnlyForDeploymentPostprocessing = 0;
		}};
""",
)
insert_section(
    "PBXSourcesBuildPhase",
    f"""
		{sources_phase_id} /* Sources */ = {{
			isa = PBXSourcesBuildPhase;
			buildActionMask = 2147483647;
			files = (
				{build_file_id} /* {source_path.name} in Sources */,
			);
			runOnlyForDeploymentPostprocessing = 0;
		}};
""",
)
insert_section(
    "PBXContainerItemProxy",
    f"""
		{proxy_id} /* PBXContainerItemProxy */ = {{
			isa = PBXContainerItemProxy;
			containerPortal = {project_object_id} /* Project object */;
			proxyType = 1;
			remoteGlobalIDString = {app_target_id};
			remoteInfo = {app_target_name};
		}};
""",
)
insert_section(
    "PBXTargetDependency",
    f"""
		{dependency_id} /* PBXTargetDependency */ = {{isa = PBXTargetDependency; target = {app_target_id} /* {app_target_name} */; targetProxy = {proxy_id} /* PBXContainerItemProxy */; }};
""",
)
insert_section(
    "XCBuildConfiguration",
    f"""
		{debug_config_id} /* Debug */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				CLANG_ENABLE_MODULES = YES;
				CODE_SIGNING_ALLOWED = NO;
				GENERATE_INFOPLIST_FILE = YES;
				IPHONEOS_DEPLOYMENT_TARGET = 16.4;
				LD_RUNPATH_SEARCH_PATHS = (
					"$(inherited)",
					"@executable_path/Frameworks",
					"@loader_path/Frameworks",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "{test_bundle_identifier}";
				PRODUCT_NAME = "$(TARGET_NAME)";
				SDKROOT = iphoneos;
				SWIFT_VERSION = 5.0;
				TARGETED_DEVICE_FAMILY = 1;
				TEST_TARGET_NAME = meeterm;
			}};
			name = Debug;
		}};
		{release_config_id} /* Release */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				CLANG_ENABLE_MODULES = YES;
				CODE_SIGNING_ALLOWED = NO;
				GENERATE_INFOPLIST_FILE = YES;
				IPHONEOS_DEPLOYMENT_TARGET = 16.4;
				LD_RUNPATH_SEARCH_PATHS = (
					"$(inherited)",
					"@executable_path/Frameworks",
					"@loader_path/Frameworks",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "{test_bundle_identifier}";
				PRODUCT_NAME = "$(TARGET_NAME)";
				SDKROOT = iphoneos;
				SWIFT_VERSION = 5.0;
				TARGETED_DEVICE_FAMILY = 1;
				TEST_TARGET_NAME = meeterm;
			}};
			name = Release;
		}};
""",
)
insert_section(
    "XCConfigurationList",
    f"""
		{target_config_list_id} /* Build configuration list for PBXNativeTarget "meetermTests" */ = {{
			isa = XCConfigurationList;
			buildConfigurations = (
				{debug_config_id} /* Debug */,
				{release_config_id} /* Release */,
			);
			defaultConfigurationIsVisible = 0;
			defaultConfigurationName = Release;
		}};
""",
)
insert_section(
    "XCBuildConfiguration",
    f"""
		{storage_debug_config_id} /* Storage Debug */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				BUNDLE_LOADER = "$(TEST_HOST)";
				CLANG_ENABLE_MODULES = YES;
				CODE_SIGNING_ALLOWED = NO;
				GENERATE_INFOPLIST_FILE = YES;
				IPHONEOS_DEPLOYMENT_TARGET = 16.4;
				LD_RUNPATH_SEARCH_PATHS = (
					"$(inherited)",
					"@executable_path/Frameworks",
					"@loader_path/Frameworks",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "{storage_bundle_identifier}";
				PRODUCT_NAME = "$(TARGET_NAME)";
				SDKROOT = iphoneos;
				SWIFT_VERSION = 5.0;
				TARGETED_DEVICE_FAMILY = 1;
				TEST_HOST = "$(BUILT_PRODUCTS_DIR)/{app_target_name}.app/{app_target_name}";
			}};
			name = Debug;
		}};
		{storage_release_config_id} /* Storage Release */ = {{
			isa = XCBuildConfiguration;
			buildSettings = {{
				BUNDLE_LOADER = "$(TEST_HOST)";
				CLANG_ENABLE_MODULES = YES;
				CODE_SIGNING_ALLOWED = NO;
				GENERATE_INFOPLIST_FILE = YES;
				IPHONEOS_DEPLOYMENT_TARGET = 16.4;
				LD_RUNPATH_SEARCH_PATHS = (
					"$(inherited)",
					"@executable_path/Frameworks",
					"@loader_path/Frameworks",
				);
				PRODUCT_BUNDLE_IDENTIFIER = "{storage_bundle_identifier}";
				PRODUCT_NAME = "$(TARGET_NAME)";
				SDKROOT = iphoneos;
				SWIFT_VERSION = 5.0;
				TARGETED_DEVICE_FAMILY = 1;
				TEST_HOST = "$(BUILT_PRODUCTS_DIR)/{app_target_name}.app/{app_target_name}";
			}};
			name = Release;
		}};
""",
)
insert_section(
    "XCConfigurationList",
    f"""
		{storage_target_config_list_id} /* Build configuration list for PBXNativeTarget "meetermStorageTests" */ = {{
			isa = XCConfigurationList;
			buildConfigurations = (
				{storage_debug_config_id} /* Storage Debug */,
				{storage_release_config_id} /* Storage Release */,
			);
			defaultConfigurationIsVisible = 0;
			defaultConfigurationName = Release;
		}};
""",
)


def add_child(group_id: str, child: str) -> None:
    global project
    object_pattern = re.compile(
        rf"(^\s*{re.escape(group_id)}(?: /\*[^*]*\*/)? = \{{.*?^\s*\);)",
        re.MULTILINE | re.DOTALL,
    )
    match = object_pattern.search(project)
    if match is None:
        raise SystemExit(f"iOS UI target injection could not find group {group_id}")
    block = match.group(1)
    if child in block:
        return
    block = block[:-2] + f"\n\t\t\t\t{child},\n\t\t\t);"
    project = project[:match.start(1)] + block + project[match.end(1):]


add_child(main_group_id, f"{tests_group_id} /* meetermTests */")
add_child(products_group_id, f"{product_ref_id} /* meetermTests.xctest */")
add_child(main_group_id, f"{storage_group_id} /* meetermStorageTests */")
add_child(products_group_id, f"{storage_product_ref_id} /* meetermStorageTests.xctest */")

generated_tests_dir.mkdir(parents=True, exist_ok=True)
generated_storage_dir.mkdir(parents=True, exist_ok=True)
shutil.copyfile(source_path, generated_tests_dir / source_path.name)

# Run focused UIKit input tests in the disposable test runner using the exact
# production input view and key enum. No Rust registry/runtime is copied into
# this runner; the real SSH UI test still exercises the app's native package.
for index, extra_source in enumerate(ui_extra_sources):
    extra_ref_id = f"00E357{index:018X}"
    extra_build_id = f"00E358{index:018X}"
    if extra_ref_id in project or extra_build_id in project:
        raise SystemExit("a reserved native input test source ID is already in use")
    shutil.copyfile(extra_source, generated_tests_dir / extra_source.name)
    insert_section(
        "PBXBuildFile",
        f"\n\t\t{extra_build_id} /* {extra_source.name} in Sources */ = {{isa = PBXBuildFile; fileRef = {extra_ref_id}; }};\n",
    )
    insert_section(
        "PBXFileReference",
        f'\n\t\t{extra_ref_id} /* {extra_source.name} */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = {extra_source.name}; sourceTree = "<group>"; }};\n',
    )
    add_child(tests_group_id, f"{extra_ref_id} /* {extra_source.name} */")
    add_child(sources_phase_id, f"{extra_build_id} /* {extra_source.name} in Sources */")

shutil.copyfile(storage_test_source, generated_storage_dir / storage_test_source.name)

project = patch_project_with_simulator_flags(
    project,
    project_path,
    app_target_id,
    app_target_name,
    app_entitlements,
)

project_object_pattern = re.compile(
    rf"(^\s*{re.escape(project_object_id)} /\* Project object \*/ = \{{.*?^\s*\}};\n/\* End PBXProject section \*/)",
    re.MULTILINE | re.DOTALL,
)
project_object_match = project_object_pattern.search(project)
if project_object_match is None:
    raise SystemExit("iOS UI target injection could not update the project object")
project_block = project_object_match.group(1)
project_block = project_block.replace(
    "\t\t\t\tTargetAttributes = {\n",
    f"\t\t\t\tTargetAttributes = {{\n\t\t\t\t\t{target_id} = {{\n\t\t\t\t\t\tTestTargetID = {app_target_id};\n\t\t\t\t\t}};\n",
    1,
)
project_block = project_block.replace(
    f"\t\t\t\t{target_id} = {{\n\t\t\t\t\tTestTargetID = {app_target_id};\n\t\t\t\t}};\n",
    f"\t\t\t\t{target_id} = {{\n\t\t\t\t\tTestTargetID = {app_target_id};\n\t\t\t\t}};\n\t\t\t\t{storage_target_id} = {{\n\t\t\t\t\tTestTargetID = {app_target_id};\n\t\t\t\t}};\n",
    1,
)
project_block = project_block.replace(
    f"\t\t\ttargets = (\n\t\t\t\t{app_target_id} /* {app_target_name} */,\n",
    f"\t\t\ttargets = (\n\t\t\t\t{app_target_id} /* {app_target_name} */,\n\t\t\t\t{target_id} /* meetermTests */,\n\t\t\t\t{storage_target_id} /* meetermStorageTests */,\n",
    1,
)
project = project[:project_object_match.start(1)] + project_block + project[project_object_match.end(1):]

scheme = scheme_path.read_text(encoding="utf-8")
scheme = re.sub(
    r"(<TestAction\n\s+buildConfiguration = )\"Debug\"",
    r'\1"Release"',
    scheme,
    count=1,
)
if f'BlueprintIdentifier = "{target_id}"' not in scheme:
    raise SystemExit("the generated scheme does not reference the reserved UI target")
if f'BlueprintIdentifier = "{storage_target_id}"' not in scheme:
    storage_reference = f'''         <TestableReference
            skipped = "NO">
            <BuildableReference
               BuildableIdentifier = "primary"
               BlueprintIdentifier = "{storage_target_id}"
               BuildableName = "meetermStorageTests.xctest"
               BlueprintName = "meetermStorageTests"
               ReferencedContainer = "container:meeterm.xcodeproj">
            </BuildableReference>
         </TestableReference>
'''
    testables_end = scheme.find("      </Testables>")
    if testables_end < 0:
        raise SystemExit("the generated scheme has no Testables section")
    scheme = scheme[:testables_end] + storage_reference + scheme[testables_end:]
scheme = re.sub(
    r'(<TestableReference\n\s+skipped = "NO")>',
    r'\1\n            parallelizable = "NO">',
    scheme,
)
podfile_path = project_path.parent.parent / "Podfile"
patched_podfile = patch_podfile_with_storage_target(podfile_path, app_target_name)
scheme_path.write_text(scheme, encoding="utf-8")
project_path.write_text(project, encoding="utf-8")
podfile_path.write_text(patched_podfile, encoding="utf-8")
print("Injected disposable meetermTests and meetermStorageTests targets.")
