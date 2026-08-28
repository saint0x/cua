from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

Json = Any


@dataclass(frozen=True)
class OwnerSession:
    session_id: str
    raw: Json


class Cua:
    def __init__(
        self,
        profile: str = "default",
        bin: str = "cua",
        env: dict[str, str] | None = None,
        transport: Literal["unix", "cli"] = "unix",
    ) -> None:
        self.profile = profile
        self.bin = bin
        self.env = {**os.environ, **(env or {})}
        self.transport = transport

    @classmethod
    def connect(
        cls,
        profile: str = "default",
        bin: str = "cua",
        env: dict[str, str] | None = None,
        transport: Literal["unix", "cli"] = "unix",
    ) -> "Cua":
        return cls(profile=profile, bin=bin, env=env, transport=transport)

    def run(self, file: str | Path, trace_dir: str | Path | None = None) -> Json:
        args = ["--profile", self.profile, "run", str(file), "--json"]
        if trace_dir is not None:
            args.extend(["--trace-dir", str(trace_dir)])
        return self._exec_json(args)

    def run_inline(self, runebook_toml: str, trace_dir: str | Path | None = None) -> Json:
        with tempfile.TemporaryDirectory(prefix="cua-runebook-") as directory:
            path = Path(directory) / f"{uuid.uuid4()}.cua.toml"
            path.write_text(runebook_toml, encoding="utf-8")
            return self.run(path, trace_dir=trace_dir)

    def run_steps(
        self,
        steps: list[dict[str, Json]],
        *,
        name: str = "python-sdk",
        trace: bool = False,
        on_error: Literal["stop", "continue", "ask", "rollback"] | None = None,
        trace_dir: str | Path | None = None,
    ) -> Json:
        runebook = _render_runebook(
            profile=self.profile,
            name=name,
            trace=trace,
            on_error=on_error,
            steps=steps,
        )
        return self.run_inline(runebook, trace_dir=trace_dir)

    def rpc(self, method: str, params: Json | None = None, session_id: str | None = None) -> Json:
        if self.transport == "unix":
            try:
                return self._unix_rpc(method, params or {}, session_id=session_id)
            except (FileNotFoundError, ConnectionRefusedError):
                pass
        session_line = f"session_id = {_toml_string(session_id)}" if session_id else ""
        runebook = "\n".join(
            line
            for line in [
                'schema = "cua.runebook.v1"',
                "",
                "[run]",
                'name = "python-rpc"',
                f"profile = {_toml_string(self.profile)}",
                "trace = false",
                "",
                "[[steps]]",
                'id = "rpc"',
                'save_as = "rpc"',
                'do = "rpc"',
                f"method = {_toml_string(method)}",
                f"params = {_toml_value(params or {})}",
                session_line,
            ]
            if line
        )
        report = self.run_inline(runebook)
        try:
            return report["results"]["rpc"]
        except KeyError as error:
            raise RuntimeError("cua runebook response did not include results.rpc") from error

    def manifest(self) -> Json:
        return self._step("manifest", {}, "manifest")

    def schemas(self) -> Json:
        return self._step("schemas", {}, "schemas")

    def metrics(self) -> Json:
        return self._step("metrics", {}, "metrics")

    def status(self) -> Json:
        return self._exec_json(["--profile", self.profile, "status", "--json"])

    def config_status(self) -> Json:
        return self._exec_json(["--profile", self.profile, "config", "status", "--json"])

    def attest(self, audience: str, nonce: str, session: OwnerSession | str | None = None) -> Json:
        return self.rpc(
            "attestation.sign",
            {
                "schema_version": "cua.v1",
                "audience": audience,
                "nonce": nonce,
            },
            session_id=_session_id(session) if session is not None else None,
        )

    def acquire_owner(self, client_name: str = "python sdk", ttl_ms: int | None = None) -> OwnerSession:
        session_id = str(uuid.uuid4())
        args = [
            "--profile",
            self.profile,
            "session",
            "acquire",
            session_id,
            "--role",
            "owner",
            "--client-name",
            client_name,
            "--json",
        ]
        if ttl_ms is not None:
            args.extend(["--ttl-ms", str(ttl_ms)])
        raw = self._exec_json(args)
        return OwnerSession(session_id=session_id, raw=raw)

    def cancel_session(self, session: OwnerSession | str, target_session_id: str | None = None) -> Json:
        return self.rpc(
            "session.cancel",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "session_id": _session_id(session),
                    "target_session_id": target_session_id,
                }
            ),
        )

    def heartbeat_owner(self, session: OwnerSession | str, ttl_ms: int | None = None) -> OwnerSession:
        raw = self.rpc(
            "session.heartbeat",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "session_id": _session_id(session),
                    "ttl_ms": ttl_ms,
                }
            ),
        )
        return OwnerSession(session_id=raw["session"]["session_id"], raw=raw)

    def session_status(self) -> Json:
        return self.rpc("session.status")

    def inbox_publish(
        self,
        text: str,
        *,
        source: str = "python-sdk",
        idempotency_key: str | None = None,
        payload: Json | None = None,
        reply_url: str | None = None,
        ttl_ms: int | None = None,
    ) -> Json:
        return self.rpc(
            "inbox.publish",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "idempotency_key": idempotency_key or str(uuid.uuid4()),
                    "source": source,
                    "text": text,
                    "payload": payload or {},
                    "reply_mode": "webhook" if reply_url else "ui",
                    "reply_url": reply_url,
                    "ttl_ms": ttl_ms,
                }
            ),
        )

    def inbox_after(self, after_sequence: int = 0) -> Json:
        return self.rpc("inbox.after", {"after_sequence": after_sequence})

    def inbox_status(self, message_id: str) -> Json:
        return self.rpc("inbox.status", {"message_id": message_id})

    def webhook_publish(
        self,
        text: str,
        *,
        source: str,
        idempotency_key: str | None = None,
        payload: Json | None = None,
        reply_url: str | None = None,
        ttl_ms: int | None = None,
    ) -> Json:
        return self.rpc(
            "webhook.publish",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "idempotency_key": idempotency_key or str(uuid.uuid4()),
                    "source": source,
                    "text": text,
                    "payload": payload or {},
                    "reply_mode": "webhook" if reply_url else "ui",
                    "reply_url": reply_url,
                    "ttl_ms": ttl_ms,
                }
            ),
        )

    def webhook_subscribe(
        self,
        source: str,
        *,
        secret: str | None = None,
        reply_url: str | None = None,
    ) -> Json:
        return self.rpc(
            "webhook.subscribe",
            {
                "schema_version": "cua.v1",
                "source": source,
                "shared_secret": secret,
                "reply_url": reply_url,
            },
        )

    def webhook_status(self, source: str) -> Json:
        return self.rpc("webhook.status", {"source": source})

    def scratchpad_write(
        self,
        name: str,
        text: str,
        session: OwnerSession | str,
        *,
        durable: bool = True,
        append: bool = False,
        ttl_ms: int | None = None,
    ) -> Json:
        return self.rpc(
            "scratchpad.write",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "name": name,
                    "text": text,
                    "durable": durable,
                    "append": append,
                    "ttl_ms": ttl_ms,
                }
            ),
            session_id=_session_id(session),
        )

    def scratchpad_read(self, name: str, durable: bool | None = None) -> Json:
        return self.rpc(
            "scratchpad.read",
            _compact(
                {
                    "schema_version": "cua.v1",
                    "name": name,
                    "durable": durable,
                }
            ),
        )

    def scratchpad_list(
        self,
        *,
        include_durable: bool = True,
        include_ephemeral: bool = True,
    ) -> Json:
        return self.rpc(
            "scratchpad.list",
            {
                "schema_version": "cua.v1",
                "include_durable": include_durable,
                "include_ephemeral": include_ephemeral,
            },
        )

    def scratchpad_delete(
        self,
        name: str,
        session: OwnerSession | str,
        *,
        durable: bool = True,
        ephemeral: bool = True,
    ) -> Json:
        return self.rpc(
            "scratchpad.delete",
            {
                "schema_version": "cua.v1",
                "name": name,
                "durable": durable,
                "ephemeral": ephemeral,
            },
            session_id=_session_id(session),
        )

    def profile_status(self) -> Json:
        return self._step("profile.status", {}, "profile")

    def create_profile(
        self,
        name: str,
        *,
        mode: Literal["observe", "supervised", "autonomous"] = "supervised",
        duration_ms: int | None = None,
        capabilities: Json | None = None,
        session: OwnerSession | str,
    ) -> Json:
        params = _compact(
            {
                "name": name,
                "mode": mode,
                "duration_ms": duration_ms,
                "capabilities": capabilities,
            }
        )
        return self.rpc("profile.create", params, session_id=_session_id(session))

    def activate_profile(self, session: OwnerSession | str) -> Json:
        return self.rpc("profile.activate", {}, session_id=_session_id(session))

    def request_accessibility(self) -> Json:
        return self._step("permissions.request_accessibility", {}, "permissions")

    def observe(self) -> Json:
        return self._step("observe", {}, "desktop")

    def screenshot(
        self,
        *,
        max_width: int | None = None,
        encoding: Literal["png", "jpeg"] | None = None,
        force_fresh: bool | None = None,
        include_bytes: bool | None = None,
    ) -> Json:
        return self._step(
            "screenshot",
            _compact(
                {
                    "max_width": max_width,
                    "encoding": encoding,
                    "force_fresh": force_fresh,
                    "include_bytes": include_bytes,
                }
            ),
            "screenshot",
        )

    def window_capture(
        self,
        window_id: int,
        *,
        max_width: int | None = None,
        encoding: Literal["png", "jpeg"] | None = None,
        include_bytes: bool | None = None,
    ) -> Json:
        return self._step(
            "window.capture",
            _compact(
                {
                    "window_id": window_id,
                    "max_width": max_width,
                    "encoding": encoding,
                    "include_bytes": include_bytes,
                }
            ),
            "window",
        )

    def context(
        self,
        *,
        max_width: int | None = None,
        encoding: Literal["png", "jpeg"] | None = None,
        force_fresh: bool | None = None,
        include_bytes: bool | None = None,
    ) -> Json:
        return self._step(
            "context",
            _compact(
                {
                    "max_width": max_width,
                    "encoding": encoding,
                    "force_fresh": force_fresh,
                    "include_bytes": include_bytes,
                }
            ),
            "context",
        )

    def events(self, *, after: int | None = None, timeout_ms: int | None = None) -> Json:
        if after is not None and timeout_ms is not None:
            return self.rpc("events.wait", {"after_sequence": after, "timeout_ms": timeout_ms})
        if after is not None:
            return self.rpc("events.after", {"after_sequence": after})
        return self.rpc("events.snapshot")

    def visual_frames(
        self,
        *,
        max_width: int = 1280,
        fps: int = 10,
        include_bytes: bool = False,
        frames: int = 3,
    ) -> Json:
        args = [
            "--profile",
            self.profile,
            "stream",
            "--unix",
            "--frames",
            str(frames),
            "--fps",
            str(fps),
            "--max-width",
            str(max_width),
            "--json",
        ]
        if include_bytes:
            args.append("--include-bytes")
        return self._exec_json(args)

    def ui_step(
        self,
        label: str,
        *,
        source: str | None = None,
        task: str | None = None,
        tool: str | None = None,
        step_index: int | None = None,
        step_total: int | None = None,
        ttl_ms: int | None = None,
    ) -> Json:
        return self._step(
            "ui.step",
            _compact(
                {
                    "label": label,
                    "source": source,
                    "task": task,
                    "tool": tool,
                    "step_index": step_index,
                    "step_total": step_total,
                    "ttl_ms": ttl_ms,
                }
            ),
            "ui",
        )

    def ui_island(self, state: Literal["expanded", "collapsed", "toggle"], source: str | None = None) -> Json:
        return self._step("ui.island", _compact({"state": state, "source": source}), "ui")

    def ui_scene_set(self, scene: Json, *, source: str | None = None) -> Json:
        return self._step("ui.scene.set", _compact({"scene": scene, "source": source}), "ui")

    def ui_scene_patch(self, scene: Json, *, source: str | None = None) -> Json:
        return self._step("ui.scene.patch", _compact({"scene": scene, "source": source}), "ui")

    def ui_scene_reset(self, *, source: str | None = None) -> Json:
        return self._step("ui.scene.reset", _compact({"source": source}), "ui")

    def ui_scene_theme(self, theme: Json, *, source: str | None = None) -> Json:
        return self._step("ui.scene.theme", _compact({"theme": theme, "source": source}), "ui")

    def ui_scene_background(self, background: Json, *, source: str | None = None) -> Json:
        return self._step("ui.scene.background", _compact({"background": background, "source": source}), "ui")

    def ui_reply(self, text: str, *, source: str | None = None, ttl_ms: int | None = None) -> Json:
        return self._step("ui.reply", _compact({"text": text, "source": source, "ttl_ms": ttl_ms}), "ui")

    def ui_mode(self, mode: Literal["headful", "headless"], source: str | None = None) -> Json:
        return self._step("ui.mode", _compact({"mode": mode, "source": source}), "ui")

    def clipboard_read(self, allow_sensitive: bool = False) -> Json:
        return self._step("clipboard.read", {"allow_sensitive": allow_sensitive}, "clipboard")

    def clipboard_write(self, text: str, session: OwnerSession | str) -> Json:
        return self.rpc(
            "clipboard.write",
            {"schema_version": "cua.v1", "text": text},
            session_id=_session_id(session),
        )

    def pause(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.pause", {}, session_id=_session_id(session))

    def resume(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.resume", {}, session_id=_session_id(session))

    def kill_switch(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.kill_switch", {}, session_id=_session_id(session))

    def dispatch(self, action: Json, session: OwnerSession | str) -> Json:
        return self.rpc("input.dispatch", action, session_id=_session_id(session))

    def dispatch_frame(self, source_frame: Json, action: Json, session: OwnerSession | str) -> Json:
        return self.rpc(
            "input.dispatch_frame",
            {"schema_version": "cua.v1", "source_frame": source_frame, "action": action},
            session_id=_session_id(session),
        )

    def visual_session(
        self,
        *,
        max_width: int | None = None,
        fps: int | None = None,
        include_bytes: bool = False,
        duration_ms: int | None = None,
        queue_depth: int | None = None,
        session: OwnerSession | str | None = None,
        timeout: float | None = None,
    ) -> "VisualSession":
        stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        if timeout is not None:
            stream.settimeout(timeout)
        stream.connect(_profile_socket_path(self.profile, self.env))
        request = {
            "id": str(uuid.uuid4()),
            "token": _profile_token(self.profile, self.env),
            "session_id": _session_id(session) if session is not None else None,
            "method": "visual.session",
            "params": _compact(
                {
                    "schema_version": "cua.v1",
                    "max_width": max_width,
                    "fps": fps,
                    "include_bytes": include_bytes,
                    "duration_ms": duration_ms,
                    "queue_depth": queue_depth,
                }
            ),
        }
        stream.sendall((json.dumps(request) + "\n").encode("utf-8"))
        return VisualSession(stream)

    def open_app(self, app: str, session: OwnerSession | str) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "kind": "open_app", "app_name": app}, session=session)

    def shell(self, command: str, session: OwnerSession | str) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "kind": "shell_exec", "command": command, "timeout_ms": 5000}, session=session)

    def aegis(self, args: list[str], session: OwnerSession | str) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "kind": "aegis", "args": args, "timeout_ms": 15000}, session=session)

    def ctx(self, args: list[str], session: OwnerSession | str) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "kind": "ctx", "args": args, "timeout_ms": 5000}, session=session)

    def trace_verify(self, dir: str | Path) -> Json:
        return self._exec_json(["--profile", self.profile, "trace", "verify", str(dir), "--json"])

    def trace_replay(self, dir: str | Path, *, dry_run: bool = False) -> Json:
        args = ["--profile", self.profile, "trace", "replay", str(dir), "--json"]
        if dry_run:
            args.append("--dry-run")
        return self._exec_json(args)

    def model_eval(
        self,
        *,
        live: bool = False,
        max_calls: int | None = None,
        max_output_tokens: int | None = None,
    ) -> Json:
        args = ["model", "eval", "--json"]
        if live:
            args.append("--live")
        if max_calls is not None:
            args.extend(["--max-calls", str(max_calls)])
        if max_output_tokens is not None:
            args.extend(["--max-output-tokens", str(max_output_tokens)])
        return self._exec_json(args)

    def _step(self, action: str, fields: dict[str, Json], save_as: str) -> Json:
        report = self.run_steps([{"id": save_as, "do": action, "save_as": save_as, **fields}])
        try:
            return report["results"][save_as]
        except KeyError as error:
            raise RuntimeError(f"cua runebook response did not include results.{save_as}") from error

    def _exec_json(self, args: list[str]) -> Json:
        result = subprocess.run(
            [self.bin, *args],
            check=False,
            env=self.env,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"cua exited {result.returncode}"
            raise RuntimeError(detail)
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"cua returned non-JSON output: {error}\n{result.stdout}") from error

    def _unix_rpc(self, method: str, params: Json, session_id: str | None = None) -> Json:
        request = {
            "id": str(uuid.uuid4()),
            "token": self._load_token(),
            "method": method,
            "params": params,
        }
        if session_id is not None:
            request["session_id"] = session_id
        stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        stream.settimeout(30)
        try:
            stream.connect(str(_profile_socket_path(self.profile, self.env)))
            stream.sendall((json.dumps(request) + "\n").encode("utf-8"))
            data = b""
            while not data.endswith(b"\n"):
                chunk = stream.recv(1 << 20)
                if not chunk:
                    break
                data += chunk
        finally:
            stream.close()
        if not data:
            raise RuntimeError(f"empty unix response for {method}")
        response = json.loads(data.decode("utf-8"))
        if response.get("ok") is not True:
            raise CuaProtocolError(method, response.get("error"))
        return response.get("result")

    def _load_token(self) -> str:
        return _profile_token(self.profile, self.env)


class CuaProtocolError(RuntimeError):
    def __init__(self, method: str, error: Json) -> None:
        super().__init__(f"cua {method} failed: {json.dumps(error)}")
        self.method = method
        self.error = error


def _render_runebook(
    *,
    profile: str,
    name: str,
    trace: bool,
    on_error: Literal["stop", "continue", "ask", "rollback"] | None,
    steps: list[dict[str, Json]],
) -> str:
    lines = [
        'schema = "cua.runebook.v1"',
        "",
        "[run]",
        f"name = {_toml_string(name)}",
        f"profile = {_toml_string(profile)}",
        f"trace = {'true' if trace else 'false'}",
    ]
    if on_error is not None:
        lines.append(f"on_error = {_toml_string(on_error)}")
    lines.append("")
    for step in steps:
        action = step["do"]
        lines.append("[[steps]]")
        lines.append(f"do = {_toml_string(action)}")
        for key, value in step.items():
            if key == "do" or value is None:
                continue
            lines.append(f"{_toml_key(key)} = {_toml_value(value)}")
        lines.append("")
    return "\n".join(lines)


class VisualSession:
    def __init__(self, stream: socket.socket) -> None:
        self._stream = stream
        self._buffer = b""
        self._closed = False

    def next_message(self) -> Json | None:
        while b"\n" not in self._buffer:
            chunk = self._stream.recv(1 << 20)
            if not chunk:
                return None
            self._buffer += chunk
        line, self._buffer = self._buffer.split(b"\n", 1)
        if not line.strip():
            return {}
        return json.loads(line.decode("utf-8"))

    def next_frame(self) -> Json | None:
        while True:
            message = self.next_message()
            if message is None:
                return None
            message_type = message.get("type")
            if message_type == "frame":
                return message["frame"]
            if message_type == "error":
                raise RuntimeError(f"visual session error: {message.get('error')}")
            if message_type == "closed":
                return None

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._stream.sendall((json.dumps({"id": str(uuid.uuid4()), "method": "visual.close", "params": {}}) + "\n").encode("utf-8"))
        while True:
            message = self.next_message()
            if message is None or message.get("type") == "closed":
                self._stream.close()
                return

    def cancel(self) -> None:
        self._closed = True
        self._stream.close()

    def __enter__(self) -> "VisualSession":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


def _session_id(session: OwnerSession | str) -> str:
    return session if isinstance(session, str) else session.session_id


def _cua_home(env: dict[str, str]) -> Path:
    configured = env.get("CUA_HOME")
    if configured:
        return Path(configured)
    return Path.home() / ".cua"


def _profile_socket_path(profile: str, env: dict[str, str]) -> Path:
    return _cua_home(env) / "profiles" / profile / "daemon.sock"


def _profile_token_path(profile: str, env: dict[str, str]) -> Path:
    return _cua_home(env) / "profiles" / profile / "http.token"


def _profile_token(profile: str, env: dict[str, str]) -> str:
    token = env.get("CUA_HTTP_TOKEN", "").strip()
    override = env.get("CUA_DEV_HTTP_TOKEN_OVERRIDE", "")
    if token and (override == "1" or override.lower() == "true"):
        return token
    path = _profile_token_path(profile, env)
    try:
        existing = path.read_text(encoding="utf-8").strip()
        if existing:
            return existing
    except FileNotFoundError:
        pass
    path.parent.mkdir(parents=True, exist_ok=True)
    created = f"cua-{uuid.uuid4()}"
    path.write_text(f"{created}\n", encoding="utf-8")
    return created


def _compact(value: dict[str, Json | None]) -> dict[str, Json]:
    return {key: field for key, field in value.items() if field is not None}


def _toml_string(value: str) -> str:
    return json.dumps(value)


def _toml_key(key: str) -> str:
    if key.replace("_", "").replace("-", "").isalnum() and not key[0].isdigit():
        return key
    return _toml_string(key)


def _toml_value(value: Json) -> str:
    if value is None:
        return "{}"
    if isinstance(value, str):
        return _toml_string(value)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int | float):
        return json.dumps(value)
    if isinstance(value, list):
        return f"[{', '.join(_toml_value(item) for item in value)}]"
    if isinstance(value, dict):
        fields = ", ".join(f"{_toml_key(str(key))} = {_toml_value(field)}" for key, field in value.items() if field is not None)
        return f"{{ {fields} }}"
    return json.dumps(value)
