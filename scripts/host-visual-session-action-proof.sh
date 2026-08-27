#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RUN_ID="$(date +%s)"
PROFILE="${CUA_VISUAL_ACTION_PROOF_PROFILE:-host-visual-action-proof-$RUN_ID}"
ADDR="${CUA_VISUAL_ACTION_PROOF_ADDR:-127.0.0.1:$((23000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-host-visual-action-proof-token-$RUN_ID}"
OUT_DIR="${CUA_VISUAL_ACTION_PROOF_OUT_DIR:-artifacts/cua/visual-action-proof-$RUN_ID}"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua >/dev/null

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
import subprocess
import sys
import time
import uuid

cua_bin, profile, addr, token, proof_path = sys.argv[1:6]
socket_path = pathlib.Path.home() / ".cua" / "profiles" / profile / "daemon.sock"
env = os.environ.copy()
env["CUA_HTTP_TOKEN"] = token
daemon = subprocess.Popen(
    [cua_bin, "--server-addr", addr, "--profile", profile, "serve", "--addr", addr],
    env=env,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)

class LineReader:
    def __init__(self, stream):
        self.stream = stream
        self.buffer = b""

    def recv_json(self):
        while b"\n" not in self.buffer:
            chunk = self.stream.recv(1 << 20)
            if not chunk:
                raise RuntimeError("visual session closed while waiting for a line")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line.decode("utf-8"))

def send_request(stream, method, params=None):
    request_id = str(uuid.uuid4())
    stream.sendall(
        (
            json.dumps(
                {
                    "id": request_id,
                    "token": token,
                    "method": method,
                    "params": params or {},
                }
            )
            + "\n"
        ).encode("utf-8")
    )
    return request_id

def wait_for_response(reader, request_id, collected_frames):
    while True:
        message = reader.recv_json()
        if message.get("type") == "frame":
            collected_frames.append(message)
            continue
        if message.get("type") == "error":
            raise RuntimeError(message)
        if message.get("id") == request_id:
            if not message.get("ok"):
                raise RuntimeError(message)
            return message["result"]

def wait_for_frame(reader):
    while True:
        message = reader.recv_json()
        if message.get("type") == "frame":
            return message
        if message.get("type") == "error":
            raise RuntimeError(message)

def frame_to_display(frame, x, y):
    envelope = frame["frame"]["envelope"]
    expected_x = round(
        envelope.get("display_x", 0)
        + ((x - envelope.get("frame_origin_x", 0)) * (envelope["display_width"] / envelope["width"]))
    )
    expected_y = round(
        envelope.get("display_y", 0)
        + ((y - envelope.get("frame_origin_y", 0)) * (envelope["display_height"] / envelope["height"]))
    )
    return expected_x, expected_y

try:
    for _ in range(3000):
        if socket_path.exists():
            try:
                probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                probe.connect(str(socket_path))
                probe.close()
                break
            except OSError:
                pass
        time.sleep(0.001)
    else:
        raise RuntimeError("daemon socket did not become ready")

    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(float(os.environ.get("CUA_VISUAL_ACTION_PROOF_TIMEOUT_SECS", "45")))
    stream.connect(str(socket_path))
    reader = LineReader(stream)
    started_at = time.perf_counter()
    send_request(
        stream,
        "visual.session",
        {
            "schema_version": "cua.v1",
            "max_width": 640,
            "fps": 20,
            "include_bytes": False,
        },
    )
    started = reader.recv_json()
    first_frame = wait_for_frame(reader)
    frame_after_start_ms = (time.perf_counter() - started_at) * 1000

    source_frame = first_frame["frame"]["envelope"]
    frame_x = min(100, source_frame["width"] - 1)
    frame_y = min(100, source_frame["height"] - 1)
    expected_x, expected_y = frame_to_display(first_frame, frame_x, frame_y)
    frames_seen_during_action = []
    action_id = send_request(
        stream,
        "input.dispatch_frame",
        {
            "schema_version": "cua.v1",
            "source_frame": source_frame,
            "action": {
                "kind": "mouse_move",
                "x": frame_x,
                "y": frame_y,
                "duration_ms": 0,
            },
        },
    )
    action = wait_for_response(reader, action_id, frames_seen_during_action)
    observe_id = send_request(stream, "observe.desktop", {})
    desktop = wait_for_response(reader, observe_id, frames_seen_during_action)
    post_action_frame = wait_for_frame(reader)
    close_id = send_request(stream, "visual.close", {})
    closed = reader.recv_json()

    cursor = desktop["cursor"]
    cursor_x = round(cursor["x"])
    cursor_y = round(cursor["y"])
    proof = {
        "schema_version": "cua.visual_action_proof.v1",
        "ok": True,
        "profile": profile,
        "started_type": started.get("type"),
        "closed_type": closed.get("type"),
        "first_frame_ms": frame_after_start_ms,
        "first_frame": {
            "frame_id": source_frame["frame_id"],
            "width": source_frame["width"],
            "height": source_frame["height"],
            "display_x": source_frame.get("display_x", 0),
            "display_y": source_frame.get("display_y", 0),
            "display_width": source_frame["display_width"],
            "display_height": source_frame["display_height"],
            "frame_origin_x": source_frame.get("frame_origin_x", 0),
            "frame_origin_y": source_frame.get("frame_origin_y", 0),
        },
        "frame_action": {
            "frame_x": frame_x,
            "frame_y": frame_y,
            "expected_display_x": expected_x,
            "expected_display_y": expected_y,
            "effect": action["effect"],
            "route": action["route"],
            "delivery_mode": action["delivery_mode"],
            "evidence": action["evidence"],
        },
        "cursor_after_action": {
            "x": cursor_x,
            "y": cursor_y,
            "visible": cursor["visible"],
        },
        "post_action_frame_id": post_action_frame["frame"]["envelope"]["frame_id"],
        "frames_seen_during_action": len(frames_seen_during_action),
    }
    proof["ok"] = (
        proof["started_type"] == "started"
        and proof["closed_type"] == "closed"
        and proof["frame_action"]["effect"] == "confirmed"
        and proof["frame_action"]["route"] == "accessibility"
        and proof["cursor_after_action"]["x"] == expected_x
        and proof["cursor_after_action"]["y"] == expected_y
        and proof["post_action_frame_id"] >= proof["first_frame"]["frame_id"]
    )
    pathlib.Path(proof_path).write_text(json.dumps(proof, indent=2) + "\n")
    print(pathlib.Path(proof_path).parent)
    if not proof["ok"]:
        raise SystemExit(json.dumps(proof, indent=2))
except Exception as error:
    proof = {
        "schema_version": "cua.visual_action_proof.v1",
        "ok": False,
        "profile": profile,
        "error": str(error),
    }
    pathlib.Path(proof_path).write_text(json.dumps(proof, indent=2) + "\n")
    print(pathlib.Path(proof_path).parent)
    raise
finally:
    try:
        stream.close()
    except Exception:
        pass
    daemon.terminate()
    try:
        daemon.wait(timeout=2)
    except subprocess.TimeoutExpired:
        daemon.kill()
PY
