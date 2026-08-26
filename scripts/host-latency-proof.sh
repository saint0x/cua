#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RUN_ID="$(date +%s)"
PROFILE="${CUA_LATENCY_PROOF_PROFILE:-host-latency-proof-$RUN_ID}"
ADDR="${CUA_LATENCY_PROOF_ADDR:-127.0.0.1:$((22000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-host-latency-proof-token-$RUN_ID}"
OUT_DIR="${CUA_LATENCY_PROOF_OUT_DIR:-artifacts/cua/latency-proof-$RUN_ID}"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua -p cua-voice >/dev/null

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

python3 - "$CUA_BIN_PATH" "$PROFILE" "$ADDR" "$TOKEN" "$PROOF" <<'PY'
import json
import os
import pathlib
import socket
import statistics
import subprocess
import sys
import time
import uuid

cua_bin, profile, addr, token, proof_path = sys.argv[1:6]
socket_path = pathlib.Path.home() / ".cua" / "profiles" / profile / "daemon.sock"
env = os.environ.copy()
env["CUA_HTTP_TOKEN"] = token
started = time.perf_counter()
daemon = subprocess.Popen(
    [cua_bin, "--server-addr", addr, "--profile", profile, "serve", "--addr", addr],
    env=env,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)

def percentile(values, pct):
    values = sorted(values)
    return values[min(len(values) - 1, int(round((pct / 100) * (len(values) - 1))))]

def stats(values):
    return {
        "p50": statistics.median(values),
        "p90": percentile(values, 90),
        "p99": percentile(values, 99),
        "max": max(values),
    }

def persistent_call(stream, method, params=None):
    request = {
        "id": str(uuid.uuid4()),
        "token": token,
        "method": method,
        "params": params or {},
    }
    sent = time.perf_counter()
    stream.sendall((json.dumps(request) + "\n").encode("utf-8"))
    line = b""
    while not line.endswith(b"\n"):
        line += stream.recv(1 << 20)
    received = time.perf_counter()
    response = json.loads(line.decode("utf-8"))
    if not response.get("ok"):
        raise RuntimeError(response)
    return (received - sent) * 1000, response["result"]

def line(stream):
    value = b""
    while not value.endswith(b"\n"):
        value += stream.recv(1 << 20)
    return json.loads(value.decode("utf-8"))

try:
    ready = None
    for _ in range(3000):
        if socket_path.exists():
            try:
                probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                probe.connect(str(socket_path))
                probe.close()
                ready = time.perf_counter()
                break
            except OSError:
                pass
        time.sleep(0.001)
    if ready is None:
        raise RuntimeError("daemon socket did not become ready")

    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.connect(str(socket_path))
    _, events = persistent_call(stream, "events.snapshot")
    sequence = max([event.get("sequence", 0) for event in events] or [0])

    step_rtt = []
    event_wait = []
    for index in range(80):
        rtt, _ = persistent_call(
            stream,
            "ui.step",
            {
                "schema_version": "cua.v1",
                "label": f"latency step {index}",
                "source": "latency proof",
                "task": "latency",
                "tool": "unix",
                "step_index": 1,
                "step_total": 1,
                "ttl_ms": 1000,
            },
        )
        step_rtt.append(rtt)
        wait_started = time.perf_counter()
        _, new_events = persistent_call(
            stream,
            "events.wait",
            {"after_sequence": sequence, "timeout_ms": 500},
        )
        event_wait.append((time.perf_counter() - wait_started) * 1000)
        sequence = max([event.get("sequence", sequence) for event in new_events] or [sequence])

    control_rtt = []
    for index in range(40):
        rtt, _ = persistent_call(
            stream,
            "input.dispatch",
            {"kind": "pause" if index % 2 == 0 else "resume"},
        )
        control_rtt.append(rtt)

    screenshot_rtt = []
    for _ in range(8):
        rtt, _ = persistent_call(
            stream,
            "capture.screenshot",
            {
                "max_width": 640,
                "encoding": "png",
                "force_fresh": True,
                "include_bytes": False,
            },
        )
        screenshot_rtt.append(rtt)

    time.sleep(0.25)
    visual = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    visual.connect(str(socket_path))
    visual_request = {
        "id": str(uuid.uuid4()),
        "token": token,
        "method": "visual.session",
        "params": {
            "schema_version": "cua.v1",
            "max_width": 640,
            "fps": 30,
            "include_bytes": False,
        },
    }
    visual_started = time.perf_counter()
    visual.sendall((json.dumps(visual_request) + "\n").encode("utf-8"))
    first_type = line(visual).get("type")
    second_type = line(visual).get("type")
    visual_first_frame_ms = (time.perf_counter() - visual_started) * 1000
    visual.close()

    proof = {
        "schema_version": "cua.latency_proof.v1",
        "ok": True,
        "profile": profile,
        "cold_socket_ready_ms": (ready - started) * 1000,
        "persistent_unix_ui_step_rtt_ms": stats(step_rtt),
        "events_wait_after_publish_ms": stats(event_wait),
        "persistent_unix_control_dispatch_rtt_ms": stats(control_rtt),
        "screenshot_fresh_no_bytes_rtt_ms": stats(screenshot_rtt),
        "visual_session_first_frame_ms": visual_first_frame_ms,
        "visual_session_first_messages": [first_type, second_type],
        "thresholds": {
            "ui_step_p90_ms": 5,
            "control_dispatch_p90_ms": 5,
            "screenshot_p90_ms": 500,
            "visual_first_frame_ms": 50,
        },
    }
    proof["ok"] = (
        proof["persistent_unix_ui_step_rtt_ms"]["p90"] <= proof["thresholds"]["ui_step_p90_ms"]
        and proof["persistent_unix_control_dispatch_rtt_ms"]["p90"]
        <= proof["thresholds"]["control_dispatch_p90_ms"]
        and proof["screenshot_fresh_no_bytes_rtt_ms"]["p90"]
        <= proof["thresholds"]["screenshot_p90_ms"]
        and proof["visual_session_first_frame_ms"] <= proof["thresholds"]["visual_first_frame_ms"]
        and proof["visual_session_first_messages"] == ["started", "frame"]
    )
    pathlib.Path(proof_path).write_text(json.dumps(proof, indent=2) + "\n")
    print(pathlib.Path(proof_path).parent)
    if not proof["ok"]:
        raise SystemExit(json.dumps(proof, indent=2))
finally:
    daemon.terminate()
    try:
        daemon.wait(timeout=2)
    except subprocess.TimeoutExpired:
        daemon.kill()
PY
