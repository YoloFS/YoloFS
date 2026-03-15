import os
import stat
from dataclasses import dataclass, field
from pathlib import Path

from fs import DirEntry, FileEntry, FsEntry, SymlinkEntry
from records import FsCheckResult, OutputCheckResult, ToolCall


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
                path.write_text(entry.content or "")
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
                content = path.read_text().strip()
                result.add(FileEntry(rel, content, mode))
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

        # FileEntry with content=None means "any content is acceptable".
        # Match these loosely by path+mode, then exclude from strict diff.
        matched_actual: set[FsEntry] = set()
        matched_expected: set[FsEntry] = set()
        for exp in expected:
            if isinstance(exp, FileEntry) and exp.content is None:
                for act in actual:
                    if (
                        isinstance(act, FileEntry)
                        and act.path == exp.path
                        and (exp.mode is None or act.mode == exp.mode)
                    ):
                        matched_actual.add(act)
                        matched_expected.add(exp)
                        break

        remaining_expected = expected - matched_expected
        remaining_actual = actual - matched_actual

        missing = [
            repr(entry)
            for entry in sorted(remaining_expected - remaining_actual, key=lambda e: e.path)
        ]
        unexpected = [
            repr(entry)
            for entry in sorted(remaining_actual - remaining_expected, key=lambda e: e.path)
        ]
        failed_reasons = [f"Missing filesystem entry: {entry}" for entry in missing]
        failed_reasons.extend(
            f"Unexpected filesystem entry: {entry}" for entry in unexpected
        )
        return FsCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)
