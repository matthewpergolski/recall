#!/usr/bin/env python3
"""Interactive, duration-limited local microphone/call capture comparison."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def capture(helper, root, label, duration, combined=False):
    folder = root / label
    folder.mkdir()
    processes = []
    handles = []
    print(f"\nRECORDING {label} for {duration} seconds. Repeat: Testing 123 blue bicycle.", flush=True)
    try:
        commands = ["record-mic", "record-audio-tap"] if combined else ["record-mic"]
        for command in commands:
            out = open(folder / f"{command}.jsonl", "w")
            err = open(folder / f"{command}.log", "w")
            handles.extend([out, err])
            processes.append(subprocess.Popen(
                [str(helper), command, "--session-dir", str(folder), "--duration", str(duration)],
                env={**os.environ, "RECALL_MIC_DIAGNOSTICS": "1"},
                stdin=subprocess.DEVNULL, stdout=out, stderr=err,
            ))
        deadline = time.monotonic() + duration + 15
        for process in processes:
            code = process.wait(timeout=max(0.1, deadline - time.monotonic()))
            if code:
                raise RuntimeError(f"Capture failed ({code}); inspect {folder}. Stop if permissions were denied.")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
        for process in processes:
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for handle in handles:
            handle.close()
    print(f"STOPPED. Saved {folder}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    build = Path(__file__).resolve().parents[1] / "capture-helper/.build"
    candidates = [build / "debug/recall-capture", build / "out/Products/Debug/recall-capture"]
    parser.add_argument("--helper", type=Path,
                        default=next((path for path in candidates if path.is_file()), candidates[0]))
    parser.add_argument("--seconds", type=int, choices=range(5, 61), default=15, metavar="5..60")
    args = parser.parse_args()
    if not args.helper.is_file():
        parser.error("Build the worktree helper first: swift build --package-path capture-helper")
    root = Path(tempfile.mkdtemp(prefix="recall-mic-diagnostics-"))
    print(f"Results (private audio, retained for comparison): {root}")
    print("No transcription, AI, audio routing changes, or BlackHole setup. Ctrl+C stops captures.")
    print("Keep the same microphone and speaking position throughout. No call connected yet.")
    route = input("Describe the mic selected in Phone/FaceTime and where you will speak: ")
    (root / "test.json").write_text(json.dumps({"route_description": route,
        "helper": str(args.helper.resolve()), "seconds_per_phase": args.seconds}, indent=2) + "\n")
    input("Press Enter to record the BEFORE-CALL baseline: ")
    capture(args.helper, root, "01-before-call", args.seconds)
    input("Now connect your call ON THE MAC. Once connected, press Enter (mic-only test): ")
    capture(args.helper, root, "02-connected-mic-only", args.seconds)
    input("Keep the call connected. Press Enter for mic + system audio: ")
    capture(args.helper, root, "03-connected-combined", args.seconds, combined=True)
    input("Keep the call connected. Press Enter to repeat mic-only after stopping system capture: ")
    capture(args.helper, root, "04-connected-mic-only", args.seconds)
    input("Hang up, keep the same mic, then press Enter for the AFTER-CALL baseline: ")
    capture(args.helper, root, "05-after-call", args.seconds)
    print(f"\nFinished. Share this directory with the agent: {root}")


if __name__ == "__main__":
    try:
        main()
    except (KeyboardInterrupt, EOFError):
        print("\nStopped. Existing diagnostic files retained; no recorder left running.")
    except (RuntimeError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error))
