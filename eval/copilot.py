import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from agent import Agent
from records import ToolCall


COPILOT_DIR = Path.home() / ".copilot"
SESSION_STATE_DIR = COPILOT_DIR / "session-state"
SHELL_TOOL_NAME = "bash"


@dataclass(frozen=True)
class CopilotAgent(Agent):
    name: str = "copilot"
    command: tuple[str, ...] = ("copilot",)
    newline: str = "\r"

    def is_screen_ready(self, screen: pyte.Screen) -> bool:
        lines = list(screen.display)
        last_idx = max((i for i, l in enumerate(lines) if l.strip()), default=-1)
        if last_idx < 0:
            return False
        recent = lines[max(0, last_idx - 9): last_idx + 1]
        if any("Loading environment:" in line for line in recent):
            return False
        return any(
            "Type @ to mention files" in line or "Confirm folder trust" in line
            for line in lines
        )

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        lines = list(screen.display)
        has_input_prompt = any("Type @ to mention files" in line for line in lines)
        return has_input_prompt and not self.is_busy(screen)

    def is_busy(self, screen: pyte.Screen) -> bool:
        lines = list(screen.display)
        last_idx = max((i for i, l in enumerate(lines) if l.strip()), default=-1)
        recent = lines[max(0, last_idx - 6): last_idx + 1]
        return any("Esc to cancel" in line for line in recent)

    def is_ask(self, screen: pyte.Screen) -> bool:
        lines = [line.lower() for line in screen.display]
        return any(
            "confirm folder trust" in line
            or "do you trust the files in this folder" in line
            or "allow directory access" in line
            or "do you want to add these directories to the allowed list" in line
            or "do you want to run this command?" in line
            or "do you want to edit" in line
            or "do you want to create" in line
            or "do you want to delete" in line
            for line in lines
        )

    def ask_signature(self, screen: pyte.Screen) -> str | None:
        lines = list(screen.display)
        # Scan bottom-to-top for the last outermost dialog box.
        box_end = None
        for i in range(len(lines) - 1, -1, -1):
            stripped = lines[i].strip()
            if not stripped:
                continue
            if stripped.startswith("╰"):
                box_end = i
                break
        if box_end is None:
            return None
        box_start = None
        for i in range(box_end - 1, -1, -1):
            stripped = lines[i].strip()
            if stripped.startswith("╭") and stripped.endswith("╮"):
                box_start = i
                break
        if box_start is None:
            return None
        # Use content lines only (between borders) for a stable signature.
        # This ignores both the bottom border (may be partially rendered or
        # truncated by terminal width) and any LLM output above the box.
        return "\n".join(lines[i].rstrip() for i in range(box_start + 1, box_end))

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        # Press Enter to select the highlighted option (1. Yes).
        return ""

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
        session_dir = self._latest_session_dir(cwd)
        if not session_dir:
            print(f"No session found for {cwd}")
            return None
        events_file = session_dir / "events.jsonl"
        if not events_file.exists():
            print(f"No events.jsonl found in {session_dir}")
            return None
        session_path = result_dir / "session.jsonl"
        shutil.copy2(events_file, session_path)
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
                event_type = event.get("type")
                data = event.get("data", {})

                if event_type == "tool.execution_start":
                    call_id = data.get("toolCallId", "")
                    tool_name = data.get("toolName", "")
                    args = data.get("arguments", {})
                    is_builtin = tool_name.lower() != SHELL_TOOL_NAME
                    call = ToolCall(
                        id=call_id,
                        is_builtin=is_builtin,
                        name=tool_name,
                        input=args,
                        raw=[event],
                    )
                    pending[call_id] = call
                    results.append(call)
                elif event_type == "tool.execution_complete":
                    call_id = data.get("toolCallId", "")
                    if call_id and call_id in pending:
                        pending[call_id].output = {"result": data.get("result", {})}
                        if not data.get("success", True):
                            pending[call_id].is_error = True
                        pending[call_id].raw.append(event)

        return results

    def _latest_session_dir(self, cwd: Path) -> Path | None:
        if not SESSION_STATE_DIR.exists():
            return None
        cwd_str = str(cwd.resolve())
        best: Path | None = None
        best_time: str | None = None
        for session_dir in SESSION_STATE_DIR.iterdir():
            if not session_dir.is_dir():
                continue
            workspace = session_dir / "workspace.yaml"
            if not workspace.exists():
                continue
            try:
                data = _parse_workspace_yaml(workspace)
                if data.get("cwd") != cwd_str:
                    continue
                updated_at = data.get("updated_at", "")
                if best_time is None or updated_at > best_time:
                    best = session_dir
                    best_time = updated_at
            except Exception:
                continue
        return best


def _parse_workspace_yaml(path: Path) -> dict[str, str]:
    """Parse the simple key: value YAML format used by Copilot's workspace.yaml."""
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if ": " in line:
            key, _, value = line.partition(": ")
            result[key.strip()] = value.strip()
    return result
