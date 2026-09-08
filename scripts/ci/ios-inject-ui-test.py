#!/usr/bin/env python3
"""Inject the disposable iOS XCUITest target into an Expo CNG project."""

from __future__ import annotations

import re
import shutil
import sys
from pathlib import Path


project_path = Path(sys.argv[1])
scheme_path = Path(sys.argv[2])
source_path = Path(sys.argv[3])
project = project_path.read_text(encoding="utf-8")

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

if re.search(
    rf"^\s*{re.escape(target_id)} /\* meetermTests \*/ = \{{",
    project,
    re.MULTILINE,
):
    raise SystemExit("meetermTests already exists in the generated iOS project")
if target_id in project:
    raise SystemExit("the reserved iOS UI test target ID is already in use")

generated_tests_dir = project_path.parent.parent / "meetermTests"
generated_tests_dir.mkdir(parents=True, exist_ok=True)
shutil.copyfile(source_path, generated_tests_dir / source_path.name)


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
				PRODUCT_BUNDLE_IDENTIFIER = "dev.meeterm.app.meetermTests";
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
				PRODUCT_BUNDLE_IDENTIFIER = "dev.meeterm.app.meetermTests";
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
    f"\t\t\ttargets = (\n\t\t\t\t{app_target_id} /* {app_target_name} */,\n",
    f"\t\t\ttargets = (\n\t\t\t\t{app_target_id} /* {app_target_name} */,\n\t\t\t\t{target_id} /* meetermTests */,\n",
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
scheme_path.write_text(scheme, encoding="utf-8")
project_path.write_text(project, encoding="utf-8")
print("Injected disposable meetermTests XCUITest target.")
