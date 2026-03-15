from dataclasses import dataclass
from pathlib import Path

import pyte

from records import ToolCall

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

    def ask_signature(self, screen: pyte.Screen) -> str | None:
        """Return a stable signature for the current ask dialog.

        Return ``None`` if the dialog is not yet fully rendered – the runner
        will hold off replying until a non-``None`` value is returned.
        Override in subclasses to extract only the dialog content so that
        unrelated screen changes (spinners, streaming text) don't cause
        duplicate replies.
        """
        return "\n".join(
            line.rstrip() for line in screen.display if line.strip()
        )

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return None

    def is_waiting_for_input(self, screen: pyte.Screen) -> bool:
        return False

    def is_busy(self, screen: pyte.Screen) -> bool:
        return False
