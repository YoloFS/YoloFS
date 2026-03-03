from dataclasses import asdict, dataclass, field
from typing import Any


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
