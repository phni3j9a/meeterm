"""Fast checks for diagnostic failures that must not become acceptance."""

import base64
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from feasibility import ProbeError, Stream, replay_native


class StreamChecks(unittest.TestCase):
    def stream(self, data: bytes) -> Stream:
        reader, writer = os.pipe()
        os.write(writer, data)
        os.close(writer)
        process = SimpleNamespace(
            stdout=os.fdopen(reader, "rb"), stdin=io.BytesIO(), stderr=io.BytesIO(),
            poll=lambda: 0,
        )
        stream = Stream(process)
        self.addCleanup(stream.close)
        return stream

    @staticmethod
    def frame(seq: int, payload=b"hello") -> dict:
        return {"type": "terminal.frame", "seq": seq, "encoding": "ansi",
                "full": seq == 1, "width": 40, "height": 16,
                "bytes": base64.b64encode(payload).decode()}

    def test_coalesced_records_preserve_order_and_tail(self):
        records = [self.frame(1), self.frame(2, b"\x1b[1;1Hworld")]
        stream = self.stream(b"".join((json.dumps(record) + "\n").encode() for record in records))
        self.assertEqual(stream.record(), records[0])
        self.assertEqual(stream.record(), records[1])
        self.assertEqual(stream.frames, records)

    def test_truncated_record_is_not_a_frame(self):
        stream = self.stream(b'{"type":"terminal.frame"')
        with self.assertRaisesRegex(ProbeError, "ended before expected"):
            stream.record()
        self.assertEqual(stream.frames, [])

    def test_invalid_payload_is_not_recorded_as_native_evidence(self):
        frame = self.frame(1)
        frame["bytes"] = "not base64!"
        stream = self.stream((json.dumps(frame) + "\n").encode())
        with self.assertRaises(ValueError):
            stream.record()
        self.assertEqual(stream.frames, [])

    def test_unknown_encoding_is_not_fed_to_terminal(self):
        frame = self.frame(1)
        frame["encoding"] = "future-encoding"
        stream = self.stream((json.dumps(frame) + "\n").encode())
        with self.assertRaisesRegex(ProbeError, "unsupported"):
            stream.record()
        self.assertEqual(stream.frames, [])

    def test_closed_stream_cannot_satisfy_readiness(self):
        stream = self.stream(b'{"type":"terminal.closed","reason":"busy"}\n')
        with self.assertRaisesRegex(ProbeError, "closed before expected"):
            stream.until("READY")

    def test_zero_matching_rust_tests_is_not_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            # cargo can exit 0 when a misspelled test filter selects no tests.
            # The fresh native.json is the required completion evidence.
            with patch("feasibility.subprocess.run", return_value=SimpleNamespace(returncode=0)):
                with self.assertRaises(FileNotFoundError):
                    replay_native([self.frame(1)], Path(temporary) / "replay")


if __name__ == "__main__":
    unittest.main()
