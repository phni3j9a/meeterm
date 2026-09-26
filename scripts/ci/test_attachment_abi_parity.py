#!/usr/bin/env python3
"""Attachment adapter <-> Rust core ABI parity checks (Issue #28).

Verifies without a device or toolchain that:
- every ``Java_dev_meeterm_terminal_MeetermNative_*`` export in ``jni.rs``
  has a matching ``external fun`` on ``MeetermNative.kt`` and vice versa;
- the JNI ``attachmentSnapshot`` flat string array field count matches the
  Kotlin codec's ``FIELD_COUNT``;
- every ``meetterm_attachment_*`` prototype in ``meeterm_core.h`` is wrapped
  by the iOS adapter, and no Swift file references an undeclared
  ``meetterm_*`` symbol;
- the Kotlin and Swift operation-phase enums stay inside the header's
  declared phase range (deletion is ``flags & 0x2``, not a phase).
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
JNI_RS = ROOT / 'native/meeterm-core/src/jni.rs'
KOTLIN_NATIVE = ROOT / 'modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/MeetermNative.kt'
KOTLIN_CODEC = ROOT / 'modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/AttachmentOperation.kt'
CORE_H = ROOT / 'modules/meeterm-terminal/ios/include/meeterm_core.h'
IOS_DIR = ROOT / 'modules/meeterm-terminal/ios'
SWIFT_OP = IOS_DIR / 'AttachmentOperation.swift'

ATTACHMENT_JNI = {
    'attachmentBegin',
    'attachmentRetryUpload',
    'attachmentInsert',
    'attachmentCancel',
    'attachmentDispose',
    'attachmentDeleteRemote',
    'attachmentSnapshot',
}
ATTACHMENT_C = {
    'meeterm_attachment_begin',
    'meeterm_attachment_retry_upload',
    'meeterm_attachment_insert',
    'meeterm_attachment_cancel',
    'meeterm_attachment_dispose',
    'meeterm_attachment_delete_remote',
    'meeterm_attachment_snapshot',
    'meeterm_attachment_snapshot_size',
}


def jni_exports():
    text = JNI_RS.read_text()
    return {
        match.group(1)
        for match in re.finditer(
            r'Java_dev_meeterm_terminal_MeetermNative_([A-Za-z0-9_]+)', text)
    }


def kotlin_external_funs():
    text = KOTLIN_NATIVE.read_text()
    return set(re.findall(r'external fun ([A-Za-z0-9_]+)\s*\(', text))


def header_functions():
    """Declared C functions: ``returntype meeterm_name(`` at line start."""
    text = CORE_H.read_text()
    return {
        match.group(1)
        for match in re.finditer(r'^\w[^;\n]*?\b(meeterm_[a-z0-9_]+)\s*\(', text, re.M)
    }


def swift_used_symbols():
    used = set()
    for path in sorted(IOS_DIR.glob('*.swift')):
        for match in re.finditer(r'\b(meeterm_[a-z0-9_]+)\s*\(', path.read_text()):
            name = match.group(1)
            if name.endswith('_t'):  # imported struct initializers, not calls
                continue
            used.add(name)
    return used


class TestAttachmentAbiParity(unittest.TestCase):
    def test_jni_exports_match_kotlin_declarations(self):
        exports = jni_exports()
        declared = kotlin_external_funs()
        missing = sorted(exports - declared)
        extra = sorted(declared - exports)
        self.assertEqual([], missing, 'jni.rs exports without external fun')
        self.assertEqual([], extra, 'external fun without jni.rs export')
        self.assertTrue(
            ATTACHMENT_JNI <= exports,
            'jni.rs is missing attachment exports: '
            + str(sorted(ATTACHMENT_JNI - exports)))

    def test_snapshot_field_count_matches_jni_array(self):
        jni_text = JNI_RS.read_text()
        body = re.search(
            r'MeetermNative_attachmentSnapshot.*?new_object_array\((\d+)',
            jni_text, re.S)
        self.assertIsNotNone(body, 'attachmentSnapshot array size not found')
        match = body
        kotlin_text = KOTLIN_CODEC.read_text()
        count = re.search(r'FIELD_COUNT\s*=\s*(\d+)', kotlin_text)
        self.assertIsNotNone(count, 'Kotlin FIELD_COUNT not found')
        self.assertEqual(int(match.group(1)), int(count.group(1)))

    def test_attachment_prototypes_wrapped_by_swift(self):
        declared = header_functions()
        missing = sorted(ATTACHMENT_C - declared)
        self.assertEqual([], missing, 'header is missing attachment prototypes')
        used = swift_used_symbols()
        unwrapped = sorted(ATTACHMENT_C - used)
        self.assertEqual([], unwrapped, 'attachment ABI not wrapped by Swift')
        undeclared = sorted(used - declared)
        self.assertEqual([], undeclared, 'Swift references undeclared symbols')

    def test_phase_enums_stay_within_header_range(self):
        header = CORE_H.read_text()
        declared = {
            int(match.group(1))
            for match in re.finditer(
                r'MEETERM_ATTACHMENT_(?:PENDING|UPLOADING|UPLOADED|INSERTED|FAILED|CANCELLED)\s*=\s*(\d+)',
                header)
        }
        self.assertEqual({0, 1, 2, 3, 4, 5}, declared)
        kotlin = {
            int(match.group(1))
            for match in re.finditer(r'\b[A-Z]+\((\d+)\)', KOTLIN_CODEC.read_text())
        }
        self.assertTrue(kotlin <= declared, 'Kotlin phase outside header range')
        swift = {
            int(match.group(1))
            for match in re.finditer(r'case \w+ = (\d+)', SWIFT_OP.read_text())
        }
        self.assertTrue(swift <= declared, 'Swift phase outside header range')


if __name__ == '__main__':
    unittest.main()
