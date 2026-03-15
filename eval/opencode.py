import json
import sqlite3
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent
from scripts.records import ToolCall

OPENCODE_DB = Path.home() / ".local" / "share" / "opencode" / "opencode.db"
SHELL_TOOL_NAME = "bash"


@dataclass(frozen=True)
class OpenCodeAgent(Agent):
    name: str = "opencode"
    command: tuple[str, ...] = ("opencode",)
    newline: str = "\r"

    def is_screen_ready(self, screen: pyte.Screen) -> bool:
        # The bottom bar shows "commands" (from "ctrl+p commands") once the UI
        # is fully loaded and the input field is focused.
        return any("commands" in line for line in screen.display)

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        # The idle bottom bar shows "commands" (from the "ctrl+p commands" hint).
        lines = [line for line in screen.display]
        has_idle_hint = any("commands" in line for line in lines)
        return has_idle_hint and not self.is_busy(screen)

    def is_busy(self, screen: pyte.Screen) -> bool:
        # When processing, the bottom bar shows "esc " followed by a spinner.
        # This only appears when status.type != "idle".
        lines = [line for line in screen.display]
        return any("esc " in line for line in lines)

    def is_ask(self, screen: pyte.Screen) -> bool:
        lines = [line.lower() for line in screen.display]
        return any("permission required" in line for line in lines)

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        # Press Enter to select the first option ("Allow once").
        return "\r"

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
        session_id = self._latest_session_id(cwd)
        if not session_id:
            print(f"No session found for {cwd}")
            return None

        try:
            result = subprocess.run(
                ["opencode", "export", session_id],
                capture_output=True,
                text=True,
                check=True,
            )
        except subprocess.CalledProcessError as exc:
            print(f"Failed to export session {session_id}: {exc.stderr}")
            return None

        # stdout contains "Exporting session: ...\n{...json...}"
        # Find where the JSON object starts.
        raw = result.stdout
        json_start = raw.find("{")
        if json_start < 0:
            print(f"No JSON found in export output for session {session_id}")
            return None

        session_path = result_dir / "session.json"
        session_path.write_text(raw[json_start:])
        return session_path

    def extract_tool_calls(self, session_path: Path) -> list[ToolCall]:
        pending: dict[str, ToolCall] = {}
        results: list[ToolCall] = []

        data = json.loads(session_path.read_text(encoding="utf-8"))
        for message in data.get("messages", []):
            for part in message.get("parts", []):
                part_type = part.get("type")
                if part_type == "tool-call":
                    tool_name = part.get("toolName", "")
                    call_id = part.get("toolCallId", "")
                    is_builtin = tool_name.lower() != SHELL_TOOL_NAME
                    call = ToolCall(
                        id=call_id,
                        is_builtin=is_builtin,
                        name=tool_name,
                        input=part.get("input", {}),
                        raw=[part],
                    )
                    pending[call_id] = call
                    results.append(call)
                elif part_type in ("tool-result", "tool-error"):
                    call_id = part.get("toolCallId", "")
                    if call_id and call_id in pending:
                        pending[call_id].output = {"output": part.get("output")}
                        if part_type == "tool-error":
                            pending[call_id].is_error = True
                        pending[call_id].raw.append(part)

        return results

    def _latest_session_id(self, cwd: Path) -> str | None:
        if not OPENCODE_DB.exists():
            return None
        try:
            con = sqlite3.connect(OPENCODE_DB)
            cur = con.execute(
                "SELECT id FROM session WHERE directory = ? ORDER BY time_updated DESC LIMIT 1",
                (str(cwd.resolve()),),
            )
            row = cur.fetchone()
            con.close()
            return row[0] if row else None
        except sqlite3.Error as exc:
            print(f"DB error querying session: {exc}")
            return None
