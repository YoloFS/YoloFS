import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte


@dataclass(frozen=True)
class Agent:
    name: str
    command: tuple[str, ...]
    newline: str = "\n"
    input_delay: float = 1.0

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        pass

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        pass

    def is_screen_ready(self, screen: pyte.Screen) -> bool:
        return any(line.strip() for line in screen.display)

    def is_ask(self, screen: pyte.Screen) -> bool:
        return False

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return None

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        return False

    def is_busy(self, screen: pyte.Screen) -> bool:
        return False


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

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        jsonl_files = [
            path
            for path in project_dir.iterdir()
            if path.is_file() and path.suffix == ".jsonl"
        ]
        if not jsonl_files:
            print(f"No .jsonl file found in {project_dir}")
            return
        latest_jsonl = max(jsonl_files, key=lambda path: path.stat().st_mtime)
        conversation_path = log_dir / "conversation.jsonl"
        latest_jsonl.rename(conversation_path)
        self.extract_command_results(conversation_path, log_dir / "command.jsonl")

    def extract_command_results(
        self, conversation_path: Path, output_path: Path
    ) -> None:
        results: list[dict[str, object]] = []

        with conversation_path.open("r", encoding="utf-8") as file:
            for line in file:
                line = line.strip()
                if not line:
                    continue
                event = json.loads(line)

                message = event.get("message")
                if not isinstance(message, dict):
                    continue

                content = message.get("content")
                if not isinstance(content, list):
                    continue

                for item in content:
                    if item.get("type") not in {"tool_use", "tool_result"}:
                        continue
                    results.append(item)

        with output_path.open("w", encoding="utf-8") as file:
            for record in results:
                file.write(json.dumps(record))
                file.write("\n")
