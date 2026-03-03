from dataclasses import dataclass
from pathlib import Path

import pyte

from scripts.models import ToolCall

@dataclass(frozen=True)
class Agent:
    name: str
    command: tuple[str, ...]
    newline: str = "\n"
    input_delay: float = 1.0
    select_timeout: float = 1.0

    def prepare_run(self, cwd: Path, result_dir: Path) -> None:
        pass

    def save_session(self, cwd: Path, result_dir: Path) -> Path | None:
        return None

    def extract_tool_calls(self, session_path: Path) -> list["ToolCall"]:
        return []

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
