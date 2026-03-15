import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from agent import Agent
from records import ToolCall

GEMINI_DIR = Path.home() / ".gemini"
TRUSTED_FOLDERS_PATH = GEMINI_DIR / "trustedFolders.json"
PROJECTS_PATH = GEMINI_DIR / "projects.json"
SHELL_TOOL_NAME = "run_shell_command"


@dataclass(frozen=True)
class GeminiAgent(Agent):
    name: str = "gemini"
    command: tuple[str, ...] = ("gemini",)
    newline: str = "\r"
    select_timeout: float = 3.0
    input_delay: float = 2.0

    def _get_project_slug(self, cwd: Path) -> str | None:
        if not PROJECTS_PATH.exists():
            return None
        try:
            data = json.loads(PROJECTS_PATH.read_text())
            return data.get("projects", {}).get(str(cwd.resolve()))
        except (json.JSONDecodeError, KeyError):
            return None

    def is_screen_ready(self, screen: pyte.Screen) -> bool:
        # Idle: "? for shortcuts" alone on its line.
        # Busy: spinner + "(esc to cancel, Xs)   ? for shortcuts" combined on one line.
        return any(line.strip() == "? for shortcuts" for line in screen.display)

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        return self.is_screen_ready(screen)

    def is_busy(self, screen: pyte.Screen) -> bool:
        return any("esc to cancel" in line.lower() for line in screen.display)

    def is_ask(self, screen: pyte.Screen) -> bool:
        lines = [line.lower() for line in screen.display]
        return any(
            "do you trust this folder" in line
            or "action required" in line
            or "answer questions" in line
            for line in lines
        )

    def ask_signature(self, screen: pyte.Screen) -> str | None:
        lines = list(screen.display)
        # Scan bottom-to-top for the last dialog box (╭…╰).
        # Return None while the box is still rendering so the runner waits.
        box_end = None
        for i in range(len(lines) - 1, -1, -1):
            stripped = lines[i].strip()
            if not stripped:
                continue
            if stripped.startswith("╰") and stripped.endswith("╯"):
                box_end = i
                break
            if stripped.startswith("╰"):
                # The border may have wrapped to the next line when wider
                # than the terminal (e.g. garbled multi-byte characters).
                if i + 1 < len(lines) and "╯" in lines[i + 1]:
                    box_end = i
                    break
                return None
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
        # Use only the inner │…│ content lines as signature so that
        # screen scrolling (different line positions) doesn't create
        # a different signature for the same dialog.
        inner = []
        for i in range(box_start + 1, box_end):
            line = lines[i].strip()
            if line.startswith("│") and line.endswith("│"):
                inner.append(line)
        return "\n".join(inner)

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        lines = [line.lower() for line in screen.display]
        if any("do you trust this folder" in line for line in lines):
            return "1"
        # Tool confirmation: press Enter to accept the default selection (Allow once).
        return ""

    def prepare_run(self, cwd: Path, result_dir: Path) -> None:
        # Add cwd to trustedFolders.json so the trust dialog is skipped.
        GEMINI_DIR.mkdir(parents=True, exist_ok=True)
        if TRUSTED_FOLDERS_PATH.exists():
            try:
                trusted = json.loads(TRUSTED_FOLDERS_PATH.read_text())
            except json.JSONDecodeError:
                trusted = {}
        else:
            trusted = {}
        trusted[str(cwd.resolve())] = "TRUST_FOLDER"
        TRUSTED_FOLDERS_PATH.write_text(json.dumps(trusted, indent=2))

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
        slug = self._get_project_slug(cwd)
        if not slug:
            print(f"No project slug found for {cwd} in {PROJECTS_PATH}")
            return None
        chats_dir = GEMINI_DIR / "tmp" / slug / "chats"
        if not chats_dir.exists():
            print(f"No chats directory found at {chats_dir}")
            return None
        session_files = [
            p for p in chats_dir.iterdir()
            if p.is_file() and p.suffix == ".json" and p.name.startswith("session-")
        ]
        if not session_files:
            print(f"No session files found in {chats_dir}")
            return None
        latest = max(session_files, key=lambda p: p.stat().st_mtime)
        session_path = result_dir / "session.json"
        shutil.copy2(latest, session_path)
        return session_path

    def extract_tool_calls(self, session_path: Path) -> list[ToolCall]:
        results: list[ToolCall] = []
        data = json.loads(session_path.read_text(encoding="utf-8"))
        for message in data.get("messages", []):
            if message.get("type") != "gemini":
                continue
            for tool_call in message.get("toolCalls", []):
                call_id = tool_call.get("id", "")
                tool_name = tool_call.get("name", "")
                args = tool_call.get("args", {})
                result = tool_call.get("result")
                is_builtin = tool_name != SHELL_TOOL_NAME
                output = {"result": result} if result is not None else {}
                call = ToolCall(
                    id=call_id,
                    is_builtin=is_builtin,
                    name=tool_name,
                    input=args,
                    output=output,
                    raw=[tool_call],
                )
                results.append(call)
        return results
