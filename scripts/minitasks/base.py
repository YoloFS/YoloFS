import shutil
from dataclasses import dataclass, field
from pathlib import Path

from scripts.fs import DirEntry, FileEntry, FsEntry
from scripts.records import FsCheckResult, OutputCheckResult, ToolCall


@dataclass
class MiniTask:
    """Task where a command causes side effects and the agent must fully revert."""

    name: str
    prompt: str
    fs_dir: Path
    must_exist: list[FsEntry] = field(default_factory=list)
    must_not_exist: list[FsEntry] = field(default_factory=list)

    def prep(self, root_path: Path, cwd: Path) -> None:
        shutil.copytree(str(self.fs_dir), str(cwd), dirs_exist_ok=True)

    def check_outputs(self, tool_calls: list[ToolCall]) -> OutputCheckResult:
        return OutputCheckResult(success=True, failed_reasons=[])

    def check_fs(self, root_path: Path, cwd: Path) -> FsCheckResult:
        failed_reasons: list[str] = []

        for entry in self.must_exist:
            full = cwd / entry.path
            if not full.exists():
                failed_reasons.append(f"Missing: {entry.path}")
            elif isinstance(entry, FileEntry) and entry.content is not None:
                actual = full.read_text()
                if actual != entry.content:
                    failed_reasons.append(f"Wrong content: {entry.path}")
            elif isinstance(entry, DirEntry) and not full.is_dir():
                failed_reasons.append(f"Not a directory: {entry.path}")

        for entry in self.must_not_exist:
            full = cwd / entry.path
            if full.exists():
                failed_reasons.append(f"Should not exist: {entry.path}")

        return FsCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)
