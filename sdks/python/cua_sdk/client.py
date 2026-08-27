from __future__ import annotations

import json
import os
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
    ) -> None:
        self.profile = profile
        self.bin = bin
        self.env = {**os.environ, **(env or {})}

    @classmethod
    def connect(
        cls,
        profile: str = "default",
        bin: str = "cua",
        env: dict[str, str] | None = None,
    ) -> "Cua":
        return cls(profile=profile, bin=bin, env=env)

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
                f"do = {_toml_string(method)}",
                f"params = {_toml_value(params or {})}",
                session_line,
            ]
            if line
        )
        return self.run_inline(runebook)

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

    def acquire_owner(self, client_name: str = "python sdk", ttl_ms: int | None = None) -> OwnerSession:
        args = [
            "--profile",
            self.profile,
            "session",
            "acquire",
            "--role",
            "owner",
            "--client-name",
            client_name,
            "--json",
        ]
        if ttl_ms is not None:
            args.extend(["--ttl-ms", str(ttl_ms)])
        raw = self._exec_json(args)
        session_id = raw["session"]["session_id"]
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

    def session_status(self) -> Json:
        return self.rpc("session.status")

    def profile_status(self) -> Json:
        return self._step("profile.status", {}, "profile")

    def create_profile(
        self,
        name: str,
        *,
        mode: Literal["observe", "supervised", "autonomous"] = "supervised",
        duration_ms: int | None = None,
        capabilities: Json | None = None,
        session: OwnerSession | str | None = None,
    ) -> Json:
        params = _compact(
            {
                "name": name,
                "mode": mode,
                "duration_ms": duration_ms,
                "capabilities": capabilities,
            }
        )
        if session is not None:
            return self.rpc("profile.create", params, session_id=_session_id(session))
        return self._step("profile.create", params, "profile")

    def activate_profile(self, session: OwnerSession | str | None = None) -> Json:
        if session is not None:
            return self.rpc("profile.activate", {}, session_id=_session_id(session))
        return self._step("profile.activate", {}, "profile")

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
        return self._step("events", _compact({"after": after, "timeout_ms": timeout_ms}), "events")

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

    def ui_reply(self, text: str, *, source: str | None = None, ttl_ms: int | None = None) -> Json:
        return self._step("ui.reply", _compact({"text": text, "source": source, "ttl_ms": ttl_ms}), "ui")

    def ui_mode(self, mode: Literal["headful", "headless"], source: str | None = None) -> Json:
        return self._step("ui.mode", _compact({"mode": mode, "source": source}), "ui")

    def clipboard_read(self, allow_sensitive: bool = False) -> Json:
        return self._step("clipboard.read", {"allow_sensitive": allow_sensitive}, "clipboard")

    def clipboard_write(self, text: str, session: OwnerSession | str | None = None) -> Json:
        return self.rpc(
            "clipboard.write",
            {"schema_version": "cua.v1", "text": text},
            session_id=_session_id(session) if session is not None else None,
        )

    def pause(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.pause", {}, session_id=_session_id(session))

    def resume(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.resume", {}, session_id=_session_id(session))

    def kill_switch(self, session: OwnerSession | str) -> Json:
        return self.rpc("control.kill_switch", {}, session_id=_session_id(session))

    def dispatch(self, action: Json, session: OwnerSession | str | None = None) -> Json:
        return self.rpc("input.dispatch", action, session_id=_session_id(session) if session is not None else None)

    def dispatch_frame(self, source_frame: Json, action: Json, session: OwnerSession | str | None = None) -> Json:
        return self.rpc(
            "input.dispatch_frame",
            {"schema_version": "cua.v1", "source_frame": source_frame, "action": action},
            session_id=_session_id(session) if session is not None else None,
        )

    def open_app(self, app: str, session: OwnerSession | str | None = None) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "action": "open_app", "app": app}, session=session)

    def shell(self, command: str, session: OwnerSession | str | None = None) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "action": "shell", "command": command}, session=session)

    def aegis(self, args: list[str], session: OwnerSession | str | None = None) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "action": "aegis", "args": args}, session=session)

    def ctx(self, args: list[str], session: OwnerSession | str | None = None) -> Json:
        return self.dispatch({"schema_version": "cua.v1", "action": "ctx", "args": args}, session=session)

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


def _session_id(session: OwnerSession | str) -> str:
    return session if isinstance(session, str) else session.session_id


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
