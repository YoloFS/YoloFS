import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent
from scripts.records import ToolCall


@dataclass(frozen=True)
class ClaudeAgent(Agent):
    name: str = "claude"
    command: tuple[str, ...] = ("claude",)
    newline: str = "\r"

    def is_ask(self, screen: pyte.Screen) -> bool:
        markers = (
            "1. yes",
            "do you trust the contents of this directory",
            "is this a project you created or one you trust",
        )
        lines = [line.lower() for line in screen.display]
        return any(marker in line for line in lines for marker in markers)

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        lines = [line.lower() for line in screen.display]
        if any("1. yes" in line for line in lines):
            return "1"
        if any("y/n" in line or "(y/n)" in line or "[y/n]" in line for line in lines):
            return "y"
        return "1"

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        lines = [line.strip() for line in screen.display]
        has_prompt = any(line.startswith("❯") for line in lines)
        return has_prompt and not self.is_busy(screen)

    def is_busy(self, screen: pyte.Screen) -> bool:
        # Claude shows this footer when it's idle at the input prompt.
        if any("? for shortcuts" in line.lower() for line in screen.display):
            return False

        busy_markers = (
            "esc to interrupt",
            "…",
        )
        return any(
            marker in line.lower() for line in screen.display for marker in busy_markers
        )

    def project_dir_for_cwd(self, cwd: Path) -> Path:
        project_name = str(cwd.resolve()).replace("/", "-").replace("_", "-")
        return Path.home() / ".claude" / "projects" / project_name

    def prepare_run(self, cwd: Path, result_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        project_dir.mkdir(parents=True, exist_ok=True)
        for entry in project_dir.iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
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
        session_path = result_dir / "session.jsonl"
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
                tool_use_result = event.get("toolUseResult")
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
                        is_builtin = tool_name != "Bash"
                        call = ToolCall(
                            id=item["id"],
                            is_builtin=is_builtin,
                            name=tool_name,
                            input=item.get("input", {}),
                            cwd=cwd,
                            raw=[item],
                        )
                        pending[item["id"]] = call
                        results.append(call)
                    elif item_type == "tool_result":
                        tool_use_id = item.get("tool_use_id")
                        if tool_use_id and tool_use_id in pending:
                            call = pending[tool_use_id]
                            call.output = {"content": item.get("content")}
                            if isinstance(tool_use_result, dict):
                                call.output.update(tool_use_result)
                            elif tool_use_result is not None:
                                call.output["toolUseResult"] = tool_use_result
                            if "is_error" in item:
                                call.is_error = bool(item["is_error"])
                            call.raw.append(item)

        return results
