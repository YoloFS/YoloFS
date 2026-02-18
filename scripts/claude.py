import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent, ToolCall


@dataclass(frozen=True)
class ClaudeAgent(Agent):
    name: str = "claude"
    command: tuple[str, ...] = ("claude",)
    newline: str = "\r"

    def is_ask(self, screen: pyte.Screen) -> bool:
        return any("1. Yes" in line for line in screen.display)

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return "1"

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        lines = [line.strip() for line in screen.display]
        has_prompt = any(line.startswith("❯") for line in lines)
        return has_prompt and not self.is_busy(screen)

    def is_busy(self, screen: pyte.Screen) -> bool:
        return any("esc to interrupt" in line.lower() for line in screen.display)

    def project_dir_for_cwd(self, cwd: Path) -> Path:
        project_name = str(cwd.resolve()).replace("/", "-")
        return Path.home() / ".claude" / "projects" / project_name

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        project_dir.mkdir(parents=True, exist_ok=True)
        for entry in project_dir.iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()

    def save_session(self, cwd: Path, log_dir: Path) -> Path | None:
        project_dir = self.project_dir_for_cwd(cwd)
        jsonl_files = [
            path
            for path in project_dir.iterdir()
            if path.is_file() and path.suffix == ".jsonl"
        ]
        if not jsonl_files:
            print(f"No .jsonl file found in {project_dir}")
            return None
        latest_jsonl = max(jsonl_files, key=lambda path: path.stat().st_mtime)
        session_path = log_dir / "session.jsonl"
        shutil.copy2(latest_jsonl, session_path)
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

                cwd = event.get("cwd")
                message = event.get("message")
                if not isinstance(message, dict):
                    continue

                content = message.get("content")
                if not isinstance(content, list):
                    continue

                for item in content:
                    item_type = item.get("type")
                    if item_type == "tool_use":
                        tool_name = item["name"]
                        call = ToolCall(
                            id=item["id"],
                            name=tool_name,
                            type="command" if tool_name == "Bash" else "built-in",
                            input=item.get("input", {}),
                            cwd=cwd,
                        )
                        pending[item["id"]] = call
                        results.append(call)
                    elif item_type == "tool_result":
                        tool_use_id = item.get("tool_use_id")
                        if tool_use_id and tool_use_id in pending:
                            call = pending[tool_use_id]
                            raw = item.get("content")
                            if isinstance(raw, list):
                                call.output = "\n".join(
                                    c.get("text", "")
                                    for c in raw
                                    if isinstance(c, dict) and c.get("type") == "text"
                                )
                            else:
                                call.output = str(raw) if raw is not None else None
                            if "is_error" in item:
                                call.is_error = bool(item["is_error"])

        return results
