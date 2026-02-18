import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent, ToolCall


@dataclass(frozen=True)
class CodexAgent(Agent):
    name: str = "codex"
    command: tuple[str, ...] = ("codex",)
    newline: str = "\r"

    def sessions_dir(self) -> Path:
        return Path.home() / ".codex" / "sessions"

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        for line in screen.display:
            stripped = line.strip()
            if stripped and all(c == "─" for c in stripped):
                return True
        return any(line.lstrip().startswith("›") for line in screen.display)

    def is_busy(self, screen: pyte.Screen) -> bool:
        return any("esc to interrupt" in line.lower() for line in screen.display)

    def is_ask(self, screen: pyte.Screen) -> bool:
        for line in screen.display:
            lower = line.lower()
            if (
                "approve" in lower
                or "allow" in lower
                or "(y/n)" in lower
                or "would you like to run" in lower
                or "press enter to confirm" in lower
            ):
                return True
        return False

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return "y"

    def prepare_run(self, cwd: Path, result_dir: Path) -> None:
        sessions = self.sessions_dir()
        if sessions.exists():
            shutil.rmtree(sessions)
        sessions.mkdir(parents=True, exist_ok=True)

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
        sessions = self.sessions_dir()
        rollout_files = list(sessions.rglob("rollout-*.jsonl"))
        if not rollout_files:
            print(f"No rollout-*.jsonl file found in {sessions}")
            return None
        latest = max(rollout_files, key=lambda p: p.stat().st_mtime)
        session_path = result_dir / "session.jsonl"
        shutil.copy2(latest, session_path)
        return session_path

    def extract_tool_calls(self, session_path: Path) -> list[ToolCall]:
        pending: dict[str, ToolCall] = {}
        results: list[ToolCall] = []

        with session_path.open("r", encoding="utf-8") as file:
            for line in file:
                line = line.strip()
                if not line:
                    continue
                event = json.loads(line)

                payload = event.get("payload")
                if not isinstance(payload, dict):
                    continue

                payload_type = payload.get("type", "")
                if payload_type == "function_call":
                    try:
                        args = json.loads(payload.get("arguments", "{}"))
                    except json.JSONDecodeError:
                        args = {}
                    func_name = payload["name"]
                    is_builtin = func_name != "exec_command"
                    call = ToolCall(
                        id=payload["call_id"],
                        is_builtin=is_builtin,
                        name=func_name,
                        input=args,
                        cwd=args.get("workdir"),
                        raw=[payload],
                    )
                    pending[payload["call_id"]] = call
                    results.append(call)
                elif payload_type == "function_call_output":
                    call_id = payload.get("call_id")
                    if call_id and call_id in pending:
                        pending[call_id].output = {"output": payload.get("output")}
                        pending[call_id].raw.append(payload)

        return results
