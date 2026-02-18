from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import pyte


@dataclass
class ToolCall:
    # Matches the call with its result.
    id: str
    # False for shell commands (Bash/exec_command).
    is_builtin: bool
    # Tool name for all calls (including shell-command tools).
    name: str
    input: dict[str, Any]
    # Keyed by "content" (Claude) or "output" (Codex); merged with toolUseResult when present.
    output: dict = field(default_factory=dict)
    # None when unavailable.
    is_error: bool | None = None
    # Working directory at the time of the call, if available.
    cwd: str | None = None
    # [call_record, result_record] from the session JSONL.
    raw: list[dict] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


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
