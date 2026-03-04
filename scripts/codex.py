import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent
from scripts.records import ToolCall


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
        lines = [line.lower().strip() for line in screen.display]

        has_trust_prompt = any(
            "do you trust the contents of this directory" in line
            or "is this a project you created or one you trust" in line
            or "yes, i trust this folder" in line
            for line in lines
        )
        if has_trust_prompt:
            return True

        has_yn_prompt = any("(y/n)" in line or "[y/n]" in line for line in lines)
        if has_yn_prompt:
            return True

        has_command_approval = any(
            "would you like to run the following command?" in line for line in lines
        )
        has_command_options = any(
            "1. yes, proceed" in line
            or "press enter to confirm or esc to cancel" in line
            for line in lines
        )
        if has_command_approval and has_command_options:
            return True

        has_generic_approval = any("do you want to proceed?" in line for line in lines)
        has_generic_options = any("1. yes" in line for line in lines)
        return has_generic_approval and has_generic_options

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        lines = [line.lower() for line in screen.display]
        if any("1. yes, proceed" in line for line in lines):
            return "y"
        if any("1. yes" in line for line in lines):
            return "1"
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
                        output = payload.get("output") or ""
                        pending[call_id].output = {"output": output}
                        call = pending[call_id]
                        if call.name == "exec_command":
                            if "Process exited with code 0" not in output:
                                call.is_error = True
                        pending[call_id].raw.append(payload)

        return results
