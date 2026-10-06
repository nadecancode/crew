#!/usr/bin/env python3
"""Validate the checked Apple project graph without pretending to run Xcode.

Requires openstep-plist 0.5.2. The pinned Nix command is in CrewApp/README.md.
This is source validation, not Swift type checking or native UI acceptance.
"""

from pathlib import Path
import json
import plistlib
import re
import struct
import xml.etree.ElementTree as ET

import openstep_plist


APPLE = Path(__file__).resolve().parents[1]
project = openstep_plist.loads((APPLE / "Crew.xcodeproj/project.pbxproj").read_text())
objects = project["objects"]
root = objects[project["rootObject"]]
checks = 0


def require(condition, message):
    global checks
    if not condition:
        raise AssertionError(message)
    checks += 1


def references(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if re.fullmatch(r"[A-F0-9]{24}", key):
                require(key in objects, f"Missing object key {key}")
            references(child)
    elif isinstance(value, list):
        for child in value:
            references(child)
    elif isinstance(value, str) and re.fullmatch(r"[A-F0-9]{24}", value):
        require(value in objects, f"Unresolved object {value}")


references(project)
require(root["isa"] == "PBXProject", "Root must be PBXProject")
locations = {}


def locate(object_id, parent):
    obj = objects[object_id]
    if obj.get("sourceTree") == "BUILT_PRODUCTS_DIR":
        return
    location = parent / obj.get("path", "")
    if obj["isa"] == "PBXGroup":
        for child in obj["children"]:
            locate(child, location)
    elif obj["isa"] == "PBXFileReference":
        require(location.exists(), f"Missing file/group reference {location.relative_to(APPLE)}")
        locations[object_id] = location


locate(root["mainGroup"], APPLE)
targets = {objects[key]["name"]: objects[key] for key in root["targets"]}
require(set(targets) == {"CrewMac", "CrewMacTests", "CrewMacUITests", "CrewIOS", "CrewIOSTests", "CrewIOSUITests"}, "Six app/test targets required")
require(len(root["packageReferences"]) == 1, "Shell must resolve one real presentation package")
package = objects[root["packageReferences"][0]]
require(package["isa"] == "XCLocalSwiftPackageReference", "Presentation package must be local")
require((APPLE / package["relativePath"] / "Package.swift").is_file(), "Missing SwiftPM manifest")

expected_sources = set((APPLE / "CrewApp").glob("*.swift")) | set((APPLE / "CrewHost").glob("*.swift"))
for application, sdk, floor_key, floor in (
    ("CrewMac", "macosx", "MACOSX_DEPLOYMENT_TARGET", "14.0"),
    ("CrewIOS", "iphoneos", "IPHONEOS_DEPLOYMENT_TARGET", "17.0"),
):
    target = targets[application]
    require(target["productType"] == "com.apple.product-type.application", f"Wrong app product {application}")
    sources = set()
    for phase_id in target["buildPhases"]:
        phase = objects[phase_id]
        for build_id in phase["files"]:
            build = objects[build_id]
            if phase["isa"] == "PBXSourcesBuildPhase":
                sources.add(locations[build["fileRef"]])
            elif phase["isa"] == "PBXFrameworksBuildPhase":
                require(objects[build["productRef"]]["productName"] == "CrewUI", "No fabricated native bridge dependency")
    require(sources == expected_sources, f"Incomplete source phase for {application}")
    configurations = objects[target["buildConfigurationList"]]["buildConfigurations"]
    for configuration_id in configurations:
        settings = objects[configuration_id]["buildSettings"]
        require(settings["SDKROOT"] == sdk, f"Wrong SDK for {application}")
        require(str(settings[floor_key]) == floor, f"Wrong OS floor for {application}")
        require(str(settings["SWIFT_VERSION"]) == "5.0", "Swift 5 language mode required")
        require(settings["CODE_SIGNING_ALLOWED"] == "NO", "Foundation checks must remain signing-neutral")
        require(settings["PRODUCT_BUNDLE_IDENTIFIER"] == "dev.crew.Crew", "Coordinated local application identity required")
        require("DEVELOPMENT_TEAM" not in settings, "No signing account should be chosen")
        info = plistlib.loads((APPLE / settings["INFOPLIST_FILE"]).read_bytes())
        require(info["CFBundleDisplayName"] == "Crew", "Incorrect display name")
        require(info["CFBundlePackageType"] == "APPL", "Incorrect application bundle type")
    for suffix, product_type, source_file in (
        ("Tests", "com.apple.product-type.bundle.unit-test", {"CrewShellTests.swift", "AppleDeviceKeyVaultTests.swift"}),
        ("UITests", "com.apple.product-type.bundle.ui-testing", {"CrewShellUITests.swift"}),
    ):
        test = targets[application + suffix]
        require(test["productType"] == product_type, "Incorrect test bundle type")
        require(any(objects[dep]["target"] in root["targets"] and objects[objects[dep]["target"]]["name"] == application for dep in test["dependencies"]), "Missing test-host dependency")
        source_phase = next(objects[key] for key in test["buildPhases"] if objects[key]["isa"] == "PBXSourcesBuildPhase")
        require({locations[objects[key]["fileRef"]].name for key in source_phase["files"]} == source_file, "Wrong test source")
    scheme = ET.parse(APPLE / f"Crew.xcodeproj/xcshareddata/xcschemes/{application}.xcscheme").getroot()
    for buildable in scheme.findall(".//BuildableReference"):
        name = buildable.attrib["BlueprintName"]
        require(objects[buildable.attrib["BlueprintIdentifier"]] is targets[name], "Scheme refers to wrong target")
        require(buildable.attrib["ReferencedContainer"] == "container:Crew.xcodeproj", "Wrong scheme container")
    require({item.attrib["BlueprintName"] for item in scheme.findall("./TestAction/Testables/TestableReference/BuildableReference")} == {application + "Tests", application + "UITests"}, "Scheme must test both bundles")

ET.parse(APPLE / "Crew.xcodeproj/project.xcworkspace/contents.xcworkspacedata")
icons = APPLE / "CrewApp/Resources/Assets.xcassets/AppIcon.appiconset"
icon_spec = json.loads((icons / "Contents.json").read_text())
for image in icon_spec["images"]:
    data = (icons / image["filename"]).read_bytes()
    require(data[:8] == b"\x89PNG\r\n\x1a\n", "Icon must be a real PNG")
    width, height = struct.unpack(">II", data[16:24])
    size = int(image["size"].split("x")[0]) * int(image.get("scale", "1x")[0])
    require(width == height == size, f"Wrong icon dimensions: {image['filename']}")
    require(b"IDAT" in data, "Icon has no pixel data")
require(any(image.get("platform") == "ios" and image["size"] == "1024x1024" for image in icon_spec["images"]), "iOS icon missing")
require(len([image for image in icon_spec["images"] if image["idiom"] == "mac"]) == 10, "Mac icon sizes missing")

for asset in APPLE.rglob("*.colorset/Contents.json"):
    variants = json.loads(asset.read_text())["colors"]
    appearances = [tuple(sorted((entry["appearance"], entry["value"]) for entry in variant.get("appearances", []))) for variant in variants]
    require(len(appearances) == len(set(appearances)) == 4, f"Light/dark/high-contrast variants incomplete: {asset}")

print(f"Apple shell source graph: {checks} checks passed; {len(objects)} project objects, six targets, two shared schemes.")
print("Structural validation only. Apple compilation, simulator/device/UI execution: deferred by operator.")
