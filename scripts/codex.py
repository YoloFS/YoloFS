import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.agent import Agent


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
        return False

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

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        sessions = self.sessions_dir()
        if sessions.exists():
            shutil.rmtree(sessions)
        sessions.mkdir(parents=True, exist_ok=True)

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        sessions = self.sessions_dir()
        rollout_files = list(sessions.rglob("rollout-*.jsonl"))
        if not rollout_files:
            print(f"No rollout-*.jsonl file found in {sessions}")
            return
        latest = max(rollout_files, key=lambda p: p.stat().st_mtime)
        conversation_path = log_dir / "conversation.jsonl"
        latest.rename(conversation_path)
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

                # Codex JSONL: response_item events with payload
                payload = event.get("payload")
                if isinstance(payload, dict):
                    payload_type = payload.get("type", "")
                    if payload_type in ("function_call", "function_call_output"):
                        results.append(payload)

        with output_path.open("w", encoding="utf-8") as file:
            for record in results:
                file.write(json.dumps(record))
                file.write("\n")
