from __future__ import annotations

import json
import os
import subprocess
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

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

    def rpc(self, method: str, params: Json | None = None, session_id: str | None = None) -> Json:
        fields = _toml_fields(params or {})
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
                *fields,
                session_line,
            ]
            if line
        )
        return self.run_inline(runebook)

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


def _toml_string(value: str) -> str:
    return json.dumps(value)


def _toml_fields(value: Json) -> list[str]:
    if not isinstance(value, dict):
        return [f"value = {_toml_json(value)}"]
    return [f"{key} = {_toml_json(field)}" for key, field in value.items()]


def _toml_json(value: Json) -> str:
    if isinstance(value, str):
        return _toml_string(value)
    return json.dumps(value)
