#!/usr/bin/env python3
"""Validate the fresh iOS native-foundation observation."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import re
import sys


MIN_SURVIVAL_SECONDS = 5.0
BACKENDS = {
    "MEETERM_SMOKE_FIRST_FRAME_METAL": "metal",
    "MEETERM_SMOKE_FIRST_FRAME_SOFTWARE": "software-simulator-fallback",
}
MARKER_NAMES = {"MEETERM_SMOKE_NATIVE_READY", *BACKENDS}
COMPACT_MARKER = re.compile(
    r"^\s*(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?)"
    r"\s+\S+\s+meeterm\[(\d+):([^\]\s]+)\]\s+.*?"
    r"\b(MEETERM_SMOKE_[A-Z_]+)\s*$"
)


class ValidationError(ValueError):
    def __init__(self, reason: str) -> None:
        super().__init__(reason)
        self.reason = reason


@dataclass(frozen=True)
class Marker:
    epoch: float
    pid: str
    name: str


def _observation(path: Path) -> tuple[float, float, float]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ValidationError("observation_missing") from error
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError("malformed_observation") from error
    if not isinstance(document, dict):
        raise ValidationError("malformed_observation")

    values: list[float] = []
    for name in ("launch_epoch", "survival_start_epoch", "survival_end_epoch"):
        value = document.get(name)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValidationError("invalid_timestamps")
        value = float(value)
        if not math.isfinite(value) or value <= 0:
            raise ValidationError("invalid_timestamps")
        values.append(value)
    launch, start, end = values
    if not launch < start < end:
        raise ValidationError("invalid_timestamps")
    if end - start < MIN_SURVIVAL_SECONDS:
        raise ValidationError("survival_too_short")
    return launch, start, end


def _marker(line: str) -> Marker | None:
    if "MEETERM_SMOKE_" not in line:
        return None
    match = COMPACT_MARKER.match(line)
    if match is None:
        raise ValidationError("malformed_marker")
    date, clock, pid, _tid, name = match.groups()
    if name not in MARKER_NAMES:
        raise ValidationError("malformed_marker")
    try:
        stamp = datetime.strptime(f"{date} {clock}", "%Y-%m-%d %H:%M:%S.%f")
    except ValueError:
        try:
            stamp = datetime.strptime(f"{date} {clock}", "%Y-%m-%d %H:%M:%S")
        except ValueError as error:
            raise ValidationError("malformed_marker") from error
    return Marker(stamp.replace(tzinfo=timezone.utc).timestamp(), pid, name)


def _validate(artifact_dir: Path) -> str:
    launch, _start, end = _observation(artifact_dir / "ios-foundation-observation.json")
    try:
        lines = (artifact_dir / "simulator.log").read_text(encoding="utf-8").splitlines()
    except FileNotFoundError as error:
        raise ValidationError("log_missing") from error
    except (OSError, UnicodeError) as error:
        raise ValidationError("malformed_log") from error

    events = [event for line in lines if (event := _marker(line)) is not None]
    if not events:
        raise ValidationError("missing_marker")
    cutoff = end - MIN_SURVIVAL_SECONDS
    stale = [event for event in events if event.epoch < launch]
    late = [event for event in events if event.epoch > cutoff]
    fresh = [event for event in events if launch <= event.epoch <= cutoff]
    if not fresh:
        raise ValidationError("stale_marker" if stale else "late_marker")
    pids = {event.pid for event in fresh}
    if len(pids) != 1:
        raise ValidationError("ambiguous_pid")
    names = {event.name for event in fresh}
    if "MEETERM_SMOKE_NATIVE_READY" not in names:
        if any(event.name == "MEETERM_SMOKE_NATIVE_READY" for event in stale):
            raise ValidationError("stale_marker")
        if any(event.name == "MEETERM_SMOKE_NATIVE_READY" for event in late):
            raise ValidationError("late_marker")
        raise ValidationError("missing_native_ready")
    frame_names = names & BACKENDS.keys()
    if not frame_names:
        if any(event.name in BACKENDS for event in stale):
            raise ValidationError("stale_marker")
        if any(event.name in BACKENDS for event in late):
            raise ValidationError("late_marker")
        raise ValidationError("missing_first_frame")
    if len(frame_names) != 1:
        raise ValidationError("ambiguous_renderer")
    return BACKENDS[frame_names.pop()]


def _write_report(path: Path, result: str, reason: str, backend: str) -> None:
    path.write_text(
        f"result={result}\nreason={reason}\nrenderer_backend={backend}\n",
        encoding="utf-8",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Validate the iOS native foundation markers.")
    parser.add_argument("--artifact-dir", type=Path, required=True)
    args = parser.parse_args(argv)
    report = args.artifact_dir / "ios-foundation-validation.txt"
    try:
        backend = _validate(args.artifact_dir)
        _write_report(report, "passed", "none", backend)
        with (args.artifact_dir / "metadata.txt").open("a", encoding="utf-8") as stream:
            stream.write(f"renderer_backend={backend}\n")
    except ValidationError as error:
        try:
            _write_report(report, "failed", error.reason, "unavailable")
        except OSError:
            pass
        print(f"iOS foundation validation failed: {error.reason}", file=sys.stderr)
        return 1
    except OSError:
        try:
            _write_report(report, "failed", "artifact_write_failed", "unavailable")
        except OSError:
            pass
        print("iOS foundation validation failed: artifact_write_failed", file=sys.stderr)
        return 1
    print("iOS foundation validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
