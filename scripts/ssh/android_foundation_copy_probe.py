#!/usr/bin/env python3
"""Exercise the native foundation terminal's input and copy controls.

The probe deliberately uses the fixed ``foundation=1`` route.  It never opens
the SSH form and writes only bounded structural diagnostics; terminal contents
are evidence in screenshots, not text artifacts.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import runpy
import sys
import time


PACKAGE = "dev.meeterm.app"
KNOWN_ROW = "COPY_PROBE_29F7"
PRINTF_TEXT = "printf 'COPY29F7\\n'"
PRINTF_WHOLE = "printf 'PROBE_ASCII_123'"
FOCUS_ASCII = "FOCUS_ASCII_7B"
TERMINAL_COLUMNS_FALLBACK = 54
INPUT_SUMMARY_PATTERN = re.compile(
    r"terminal_input_summary "
    r"chunks=(?P<chunks>[0-9]+) "
    r"attemptedBytes=(?P<attempted>[0-9]+) "
    r"acceptedCommits=(?P<commits>[0-9]+) "
    r"acceptedBytes=(?P<accepted>[0-9]+) "
    r"unobservedBytes=(?P<unobserved>[0-9]+) "
    r"lastNativeCount=(?:[0-9]+|none) "
    r"rejectedCommits=(?P<rejected>[0-9]+)"
)


def load_smoke_helpers() -> dict[str, object]:
    return runpy.run_path(
        str(Path(__file__).with_name("android_smoke_impl.py")),
        run_name="android_foundation_copy_probe_helpers",
    )


def write_fixed(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(contents, encoding="utf-8")


def ime_summary(device: object) -> str:
    """Return only boolean IME state; never persist dumpsys contents."""

    run = getattr(device, "run")
    try:
        output = run(("shell", "dumpsys", "input_method"), "ime_state", timeout=10.0)
    except Exception:
        return "ime_dump=unavailable\n"
    text = output.decode("utf-8", errors="replace")
    shown = re.findall(r"mInputShown=(true|false)", text)
    window_vis = re.findall(r"mImeWindowVis=([^,\s]+)", text)
    served = bool(re.search(r"mServedInputConnection|InputConnection", text))
    return (
        f"ime_input_shown={shown[-1] if shown else 'unknown'}\n"
        f"ime_window_vis_present={1 if window_vis else 0}\n"
        f"ime_served_connection_present={1 if served else 0}\n"
    )


def latest_columns(logcat: str) -> int:
    values = [int(value) for value in re.findall(r"\bcolumns=(\d+)\b", logcat)]
    return values[-1] if values and 2 <= values[-1] <= 240 else TERMINAL_COLUMNS_FALLBACK


def parse_input_summary(logcat: str) -> dict[str, int] | None:
    match = INPUT_SUMMARY_PATTERN.search(logcat)
    if match is None:
        return None
    return {key: int(value) for key, value in match.groupdict().items()}


def reset_screen(device: object, helpers: dict[str, object], stage: str) -> None:
    wait_for_node = helpers["wait_for_node"]
    tap_node = helpers["tap_node"]
    input_text = getattr(device, "input_text")
    for sequence in ("[2J", "[H"):
        escape = wait_for_node(device, stage, text="Esc", timeout=20.0)
        tap_node(device, escape, stage)
        time.sleep(0.15)
        input_text(sequence, "terminal_input")
        time.sleep(0.2)


def clear_logcat(device: object) -> None:
    try:
        getattr(device, "run")(("shell", "logcat", "-c"), "logcat_clear", timeout=10.0)
    except Exception:
        # The filtered aggregate remains useful when logcat clear is denied.
        pass
    # AndroidDevice's sanitized aggregate uses these counters for attempted
    # bytes.  Reset them with logcat so each case reports its own input.
    setattr(device, "terminal_input_chars", 0)
    setattr(device, "terminal_input_chunks", 0)


def input_case(
    device: object,
    helpers: dict[str, object],
    output_dir: Path,
    *,
    name: str,
    value: str,
    chunked: bool,
) -> str:
    terminal_text = helpers["terminal_text"]
    # Reacquire the surface after every VT reset so the native InputConnection
    # is attached before adb emits KeyEvents.
    terminal = helpers["wait_for_labeled_terminal_surface"](
        device, f"foundation_{name}_surface", timeout=20.0
    )
    helpers["focus_terminal"](device, terminal, f"foundation_{name}_focus")
    clear_logcat(device)
    if chunked:
        terminal_text(device, value)
    else:
        getattr(device, "input_text")(value, "terminal_input")
    time.sleep(0.8)
    getattr(device, "screenshot")(output_dir / f"foundation-input-{name}.png")
    logcat = getattr(device, "logcat")()
    write_fixed(output_dir / f"foundation-input-{name}-logcat.txt", logcat)
    return (
        f"input_case_{name}=completed\n"
        f"input_case_{name}_expected_chars={len(value)}\n"
        f"input_case_{name}_chunked={1 if chunked else 0}\n"
        + ime_summary(device)
    )


def toolbar_focus_case(
    device: object,
    helpers: dict[str, object],
    output_dir: Path,
) -> str:
    """Probe toolbar focus without tapping the terminal between key events.

    The terminal is focused once while the IME is visible.  Each Esc toolbar
    action receives a tap, followed immediately by its VT bytes and ASCII;
    the probe deliberately does not call ``focus_terminal`` again.  This keeps
    the case useful for both the current APK and the qualified outer-view
    focus fix without attributing an unobserved byte to a particular cause.
    """

    stage = "foundation_toolbar_focus"
    wait_for_terminal = helpers["wait_for_labeled_terminal_surface"]
    wait_for_node = helpers["wait_for_node"]
    tap_node = helpers["tap_node"]
    terminal = wait_for_terminal(device, stage, timeout=20.0)
    helpers["focus_terminal"](device, terminal, stage)
    before = wait_for_terminal(device, stage, timeout=20.0)
    clear_logcat(device)
    # No terminal tap or focus helper is allowed between either toolbar action
    # and the committed text calls below.
    escape = wait_for_node(device, stage, text="Esc", timeout=20.0)
    tap_node(device, escape, stage)
    time.sleep(0.15)
    getattr(device, "input_text")("[2J", "terminal_input")
    escape = wait_for_node(device, stage, text="Esc", timeout=20.0)
    tap_node(device, escape, stage)
    time.sleep(0.15)
    getattr(device, "input_text")("[H", "terminal_input")
    getattr(device, "input_text")(FOCUS_ASCII, "terminal_input")
    time.sleep(0.8)
    after = wait_for_terminal(device, stage, timeout=20.0)
    getattr(device, "screenshot")(output_dir / "foundation-toolbar-focus.png")
    logcat = getattr(device, "logcat")()
    write_fixed(output_dir / "foundation-toolbar-focus-logcat.txt", logcat)
    summary = parse_input_summary(logcat)
    expected_bytes = len("[2J") + len("[H") + len(FOCUS_ASCII)
    write_fixed(
        output_dir / "foundation-toolbar-focus-geometry.txt",
        "terminal_retap_between_esc_and_input=0\n"
        "ime_hide_requested=0\n"
        + node_bounds("terminal_before", before)
        + node_bounds("terminal_after", after)
        + (
            "input_summary=present\n"
            f"input_summary_attempted_bytes={summary['attempted']}\n"
            f"input_summary_accepted_commits={summary['commits']}\n"
            f"input_summary_accepted_bytes={summary['accepted']}\n"
            f"input_summary_unobserved_bytes={summary['unobserved']}\n"
            f"input_summary_rejected_commits={summary['rejected']}\n"
            if summary is not None
            else "input_summary=absent\n"
        )
        + ime_summary(device),
    )
    smoke_failure = helpers["SmokeFailure"]
    if summary is None:
        raise smoke_failure(stage, "input_summary_unavailable")
    if summary["attempted"] != expected_bytes:
        raise smoke_failure(stage, "input_attempted_byte_mismatch")
    if summary["accepted"] != expected_bytes:
        raise smoke_failure(stage, "input_accepted_byte_mismatch")
    if summary["unobserved"] != 0:
        raise smoke_failure(stage, "input_unobserved_bytes")
    if summary["rejected"] != 0:
        raise smoke_failure(stage, "input_rejected_commits")
    return (
        "toolbar_focus_case=completed\n"
        "toolbar_focus_esc_vt_ascii_without_terminal_retap=attempted\n"
        f"toolbar_focus_expected_input_bytes={expected_bytes}\n"
        f"toolbar_focus_attempted_input_bytes={summary['attempted']}\n"
        f"toolbar_focus_accepted_input_commits={summary['commits']}\n"
        f"toolbar_focus_accepted_input_bytes={summary['accepted']}\n"
        f"toolbar_focus_unobserved_input_bytes={summary['unobserved']}\n"
        f"toolbar_focus_rejected_commits={summary['rejected']}\n"
        "toolbar_focus_history_clear_assertion=not_claimed\n"
        + ime_summary(device)
    )


def node_bounds(label: str, node: object | None) -> str:
    if node is None:
        return f"{label}=absent\n"
    bounds = getattr(node, "bounds")
    return (
        f"{label}=present\n"
        f"{label}_class={getattr(node, 'class_name', 'unavailable')}\n"
        f"{label}_left={bounds[0]}\n{label}_top={bounds[1]}\n"
        f"{label}_right={bounds[2]}\n{label}_bottom={bounds[3]}\n"
    )


def copy_case(device: object, helpers: dict[str, object], output_dir: Path) -> str:
    stage = "foundation_copy"
    wait_for_terminal = helpers["wait_for_labeled_terminal_surface"]
    focus_terminal = helpers["focus_terminal"]
    selection_drag_points = helpers["selection_drag_points"]
    wait_for_node = helpers["wait_for_node"]
    tap_node = helpers["tap_node"]

    reset_screen(device, helpers, stage)
    terminal = wait_for_terminal(device, stage, timeout=20.0)
    focus_terminal(device, terminal, stage)
    clear_logcat(device)
    getattr(device, "input_text")(KNOWN_ROW, "terminal_input")
    time.sleep(0.7)
    # A stable surface height makes the first row coordinate independent of
    # the keyboard animation.  The keyboard state is recorded after the hide.
    getattr(device, "dismiss_keyboard")(stage)
    time.sleep(0.5)
    terminal = wait_for_terminal(device, stage, timeout=20.0)
    # The foundation renderer emits its current resize geometry in sanitized
    # native logs.  Use it when available and keep the fixed API-36 fallback.
    columns = latest_columns(getattr(device, "logcat")())
    start, end = selection_drag_points(
        terminal,
        columns=columns,
        character_count=len(KNOWN_ROW),
    )
    getattr(device, "input_long_press_drag")(
        start[0], start[1], end[0], end[1], stage, duration_ms=1200
    )
    time.sleep(0.6)
    getattr(device, "screenshot")(output_dir / "foundation-selection-before-copy.png")
    write_fixed(output_dir / "foundation-copy-before-logcat.txt", getattr(device, "logcat")())

    # Record a fresh accessibility lookup after the screenshot.  This is the
    # point that distinguishes a stale node from a button that is actually
    # present at the tap coordinates.
    copy_before = wait_for_node(
        device,
        stage,
        content_description="Copy selection",
        timeout=20.0,
    )
    copy_after = wait_for_node(
        device,
        "foundation_copy_fresh_locator",
        content_description="Copy selection",
        timeout=20.0,
    )
    write_fixed(
        output_dir / "foundation-copy-node.txt",
        "fresh_locator_after_before_screenshot=1\n"
        f"columns={columns}\n"
        f"selected_chars={len(KNOWN_ROW)}\n"
        + node_bounds("terminal_before_copy", terminal)
        + node_bounds("copy_before_fresh_lookup", copy_before)
        + node_bounds("copy_after_fresh_lookup", copy_after),
    )
    tap_node(device, copy_after, "foundation_copy_tap")
    time.sleep(0.8)
    getattr(device, "screenshot")(output_dir / "foundation-selection-after-copy.png")

    # Paste through the production toolbar into the same native loopback.  A
    # screenshot is the only copy-content evidence; no clipboard text leaves
    # the emulator or enters a log/artifact.
    reset_screen(device, helpers, "foundation_paste_reset")
    terminal = wait_for_terminal(device, "foundation_paste", timeout=20.0)
    focus_terminal(device, terminal, "foundation_paste")
    paste = wait_for_node(
        device,
        "foundation_paste_locator",
        content_description="Paste",
        timeout=20.0,
    )
    write_fixed(output_dir / "foundation-paste-node.txt", node_bounds("paste", paste))
    tap_node(device, paste, "foundation_paste_tap")
    time.sleep(0.8)
    getattr(device, "screenshot")(output_dir / "foundation-paste.png")
    write_fixed(output_dir / "foundation-copy-after-paste-logcat.txt", getattr(device, "logcat")())
    return (
        "copy_selection_before_screenshot=completed\n"
        "copy_node_fresh_locator=present\n"
        "copy_tap=sent_to_fresh_locator\n"
        "copy_selection_after_screenshot=completed\n"
        "paste_tap=sent_to_production_toolbar\n"
        "paste_screenshot=completed\n"
        + ime_summary(device)
    )


def run_probe(output_dir: Path, requested_serial: str | None) -> int:
    output_dir.mkdir(parents=True, exist_ok=True)
    helpers = load_smoke_helpers()
    smoke_failure = helpers["SmokeFailure"]
    resolve_serial = helpers["resolve_serial"]
    android_device = helpers["AndroidDevice"]
    adb_path = "adb"
    details: list[str] = ["foundation=1\n", "process=alive\n"]
    device: object | None = None
    try:
        serial = resolve_serial(adb_path, requested_serial)
        device = android_device(serial, adb_path)
        device.wait_for_device()
        device.assert_process_alive("foundation_probe_start")
        terminal = helpers["wait_for_labeled_terminal_surface"](
            device, "foundation_probe_surface", timeout=20.0
        )
        helpers["focus_terminal"](device, terminal, "foundation_probe_focus")
        details.append(toolbar_focus_case(device, helpers, output_dir))
        reset_screen(device, helpers, "foundation_probe_reset")
        # These compare one adb input call with the existing 16-character
        # pacing. The split occurs after "printf 'COPY29F7", so it does not
        # exercise a loss inside the leading printf token.
        details.append(
            input_case(
                device,
                helpers,
                output_dir,
                name="printf-single",
                value=PRINTF_TEXT,
                chunked=False,
            )
        )
        reset_screen(device, helpers, "foundation_chunked_reset")
        details.append(
            input_case(
                device,
                helpers,
                output_dir,
                name="printf-16char-chunks",
                value=PRINTF_TEXT,
                chunked=True,
            )
        )
        reset_screen(device, helpers, "foundation_whole_reset")
        details.append(
            input_case(
                device,
                helpers,
                output_dir,
                name="ascii-single",
                value=PRINTF_WHOLE,
                chunked=False,
            )
        )
        details.append(copy_case(device, helpers, output_dir))
        details.append("result=completed\n")
    except smoke_failure as error:
        details.append(f"result=failed\nstage={error.stage}\nreason={error.reason}\n")
        write_fixed(output_dir / "foundation-validation.txt", "".join(details))
        return 1
    except (OSError, RuntimeError, ValueError):
        details.append("result=failed\nstage=foundation_probe\nreason=probe_error\n")
        write_fixed(output_dir / "foundation-validation.txt", "".join(details))
        return 1
    finally:
        if device is not None:
            try:
                details.append(getattr(device, "logcat")())
            except Exception:
                pass
    write_fixed(output_dir / "foundation-validation.txt", "".join(details))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-dir", type=Path, required=True)
    parser.add_argument("--serial", default=None)
    args = parser.parse_args()
    return run_probe(args.artifact_dir, args.serial)


if __name__ == "__main__":
    sys.exit(main())
