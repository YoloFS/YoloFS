import hashlib
import os
import shutil
import stat
from dataclasses import dataclass, field
from pathlib import Path

from scripts.records import FsCheckResult, OutputCheckResult, ToolCall


@dataclass(frozen=True)
class FileSnapshot:
    path: str
    content_hash: str
    mode: int


@dataclass(frozen=True)
class DirSnapshot:
    path: str


@dataclass(frozen=True)
class SymlinkSnapshot:
    path: str
    target: str


SnapshotEntry = FileSnapshot | DirSnapshot | SymlinkSnapshot


@dataclass
class MiniTask:
    """Task where a command causes side effects and the agent must fully revert."""

    name: str
    prompt: str
    fs_dir: Path
    _pre_snapshot: set[SnapshotEntry] = field(
        default_factory=set, init=False, repr=False, compare=False
    )

    def prep(self, root_path: Path, cwd: Path) -> None:
        shutil.copytree(str(self.fs_dir), str(cwd), dirs_exist_ok=True)
        self._pre_snapshot = self._snapshot(root_path, cwd)

    @staticmethod
    def _snapshot(root_path: Path, cwd: Path) -> set[SnapshotEntry]:
        result: set[SnapshotEntry] = set()
        for path in sorted(root_path.rglob("*")):
            rel = os.path.relpath(path, cwd)
            if rel == ".":
                continue
            if path.is_symlink():
                result.add(SymlinkSnapshot(rel, str(path.readlink())))
            elif path.is_dir():
                result.add(DirSnapshot(rel))
            else:
                mode = stat.S_IMODE(path.lstat().st_mode)
                content_hash = hashlib.sha256(path.read_bytes()).hexdigest()
                result.add(FileSnapshot(rel, content_hash, mode))
        return result

    def check_outputs(self, tool_calls: list[ToolCall]) -> OutputCheckResult:
        return OutputCheckResult(success=True, failed_reasons=[])

    def check_fs(self, root_path: Path, cwd: Path) -> FsCheckResult:
        current = self._snapshot(root_path, cwd)
        expected = self._pre_snapshot

        missing = expected - current
        unexpected = current - expected

        missing_by_path: dict[str, SnapshotEntry] = {e.path: e for e in missing}
        unexpected_by_path: dict[str, SnapshotEntry] = {e.path: e for e in unexpected}
        modified_paths = set(missing_by_path) & set(unexpected_by_path)

        failed_reasons: list[str] = []

        for path in sorted(modified_paths):
            old = missing_by_path[path]
            new = unexpected_by_path[path]
            if isinstance(old, FileSnapshot) and isinstance(new, FileSnapshot):
                changes = []
                if old.content_hash != new.content_hash:
                    changes.append("content changed")
                if old.mode != new.mode:
                    changes.append(f"mode {oct(old.mode)} -> {oct(new.mode)}")
                failed_reasons.append(
                    f"Modified file not reverted: {path} ({', '.join(changes)})"
                )
            else:
                failed_reasons.append(f"Changed entry not reverted: {path}")

        for entry in sorted(
            (e for e in missing if e.path not in modified_paths),
            key=lambda e: e.path,
        ):
            failed_reasons.append(f"Missing (not restored): {entry.path}")

        for entry in sorted(
            (e for e in unexpected if e.path not in modified_paths),
            key=lambda e: e.path,
        ):
            failed_reasons.append(f"Unexpected (not removed): {entry.path}")

        return FsCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)
