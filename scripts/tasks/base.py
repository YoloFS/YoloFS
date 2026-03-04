import os
import stat
from dataclasses import dataclass, field
from pathlib import Path

from scripts.records import FsCheckResult, OutputCheckResult, ToolCall


@dataclass(frozen=True)
class FileEntry:
    path: str
    content: str = ""
    mode: int | None = 0o644


@dataclass(frozen=True)
class DirEntry:
    path: str


@dataclass(frozen=True)
class SymlinkEntry:
    path: str
    target: str


FsEntry = FileEntry | DirEntry | SymlinkEntry


@dataclass
class Task:
    name: str
    prompt: str
    before: list[FsEntry] = field(default_factory=list)
    after: list[FsEntry] | None = None
    outputs: list[str] = field(default_factory=list)

    def prep(self, root_path: Path, cwd: Path) -> None:
        for entry in self.before:
            path = cwd / entry.path
            if isinstance(entry, FileEntry):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(entry.content)
                if entry.mode is not None:
                    path.chmod(entry.mode)
            elif isinstance(entry, SymlinkEntry):
                path.symlink_to(entry.target)
            elif isinstance(entry, DirEntry):
                path.mkdir(parents=True, exist_ok=True)

    @staticmethod
    def _scan_fs(root_path: Path, cwd: Path) -> set[FsEntry]:
        result: set[FsEntry] = set()
        for path in sorted(root_path.rglob("*")):
            rel = os.path.relpath(path, cwd)
            if rel == ".":
                continue
            if path.is_symlink():
                result.add(SymlinkEntry(rel, str(path.readlink())))
            elif path.is_dir():
                result.add(DirEntry(rel))
            else:
                mode = stat.S_IMODE(path.lstat().st_mode)
                result.add(FileEntry(rel, path.read_text().strip(), mode))
        return result

    def check_outputs(self, tool_calls: list[ToolCall]) -> OutputCheckResult:
        tc_outputs = [str(tc.output) for tc in tool_calls]
        failed_reasons: list[str] = []
        for expected in self.outputs:
            found = any(expected in o for o in tc_outputs)
            if not found:
                failed_reasons.append(f"Missing expected output: {expected!r}")
        return OutputCheckResult(
            success=not failed_reasons, failed_reasons=failed_reasons
        )

    def check_fs(self, root_path: Path, cwd: Path) -> FsCheckResult:
        actual = self._scan_fs(root_path, cwd)
        expected = set(self.after if self.after is not None else self.before)
        missing = [
            repr(entry) for entry in sorted(expected - actual, key=lambda e: e.path)
        ]
        unexpected = [
            repr(entry) for entry in sorted(actual - expected, key=lambda e: e.path)
        ]
        failed_reasons = [f"Missing filesystem entry: {entry}" for entry in missing]
        failed_reasons.extend(
            f"Unexpected filesystem entry: {entry}" for entry in unexpected
        )
        return FsCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)
