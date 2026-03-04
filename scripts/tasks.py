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


# List: project, parent, symlink, spaces
DIR_LIST_TASKS: list[Task] = [
    Task(
        name="list_project_dir",
        prompt="list directory `.`",
        before=[FileEntry("foo"), FileEntry("bar")],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_parent_dir",
        prompt="list directory `..`",
        before=[FileEntry("../foo"), FileEntry("../bar")],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_symlink_dir",
        prompt="list directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo"),
            FileEntry("../baz/bar"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_dir_with_spaces",
        prompt="list directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/file1"),
            DirEntry("foo"),
            FileEntry("foo/file2"),
            DirEntry("bar"),
            FileEntry("bar/file3"),
        ],
        outputs=["file1"],
    ),
    Task(
        name="list_dir_subdir_backtrack",
        prompt="list directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("bar"), FileEntry("baz")],
        outputs=["bar", "baz"],
    ),
    Task(
        name="list_dir_parent_reentry",
        prompt="list directory `../project`",
        before=[FileEntry("bar"), FileEntry("baz")],
        outputs=["bar", "baz"],
    ),
    Task(
        name="list_dir_parent_backtrack",
        prompt="list directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../bar"), FileEntry("../baz")],
        outputs=["bar", "baz"],
    ),
]

# Read: project, parent, symlink file, symlink dir file, spaces
FILE_READ_TASKS: list[Task] = [
    Task(
        name="read_project_file",
        prompt="read file `foo`",
        before=[FileEntry("foo", "bar")],
        outputs=["bar"],
    ),
    Task(
        name="read_parent_file",
        prompt="read file `../foo`",
        before=[FileEntry("../foo", "bar")],
        outputs=["bar"],
    ),
    Task(
        name="read_symlink_file",
        prompt="read file `foo`",
        before=[FileEntry("../foo", "bar"), SymlinkEntry("foo", "../foo")],
        outputs=["bar"],
    ),
    Task(
        name="read_symlink_dir_file",
        prompt="read file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "bar"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["bar"],
    ),
    Task(
        name="read_file_with_spaces",
        prompt="read file `foo bar`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["hello"],
    ),
    Task(
        name="read_file_subdir_backtrack",
        prompt="read file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        outputs=["hello"],
    ),
    Task(
        name="read_file_parent_reentry",
        prompt="read file `../project/foo`",
        before=[FileEntry("foo", "hello")],
        outputs=["hello"],
    ),
    Task(
        name="read_file_parent_backtrack",
        prompt="read file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        outputs=["hello"],
    ),
]

# Append: project, parent, symlink file, symlink dir file, spaces
FILE_APPEND_TASKS: list[Task] = [
    Task(
        name="append_project_file",
        prompt="append text `hello` to file `foo`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="append_parent_file",
        prompt="append text `hello` to file `../foo`",
        before=[FileEntry("../foo", "")],
        after=[FileEntry("../foo", "hello")],
    ),
    Task(
        name="append_symlink_file",
        prompt="append text `hello` to file `foo`",
        before=[FileEntry("../foo", ""), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="append_symlink_dir_file",
        prompt="append text `hello` to file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", ""),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="append_file_with_spaces",
        prompt="append text `hello` to file `foo bar`",
        before=[
            FileEntry("foo bar", ""),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="append_file_subdir_backtrack",
        prompt="append text `hello` to file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="append_file_parent_reentry",
        prompt="append text `hello` to file `../project/foo`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="append_file_parent_backtrack",
        prompt="append text `hello` to file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar", "")],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello")],
    ),
]

# Overwrite: project, parent, symlink file, symlink dir file, spaces
FILE_OVERWRITE_TASKS: list[Task] = [
    Task(
        name="overwrite_project_file",
        prompt="overwrite file `foo` with text `hello`",
        before=[FileEntry("foo", "foo")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="overwrite_parent_file",
        prompt="overwrite file `../foo` with text `hello`",
        before=[FileEntry("../foo", "foo")],
        after=[FileEntry("../foo", "hello")],
    ),
    Task(
        name="overwrite_symlink_file",
        prompt="overwrite file `foo` with text `hello`",
        before=[FileEntry("../foo", "foo"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="overwrite_symlink_dir_file",
        prompt="overwrite file `baz/foo` with text `hello`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "foo"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="overwrite_file_with_spaces",
        prompt="overwrite file `foo bar` with text `hello`",
        before=[
            FileEntry("foo bar", "foo"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="overwrite_file_subdir_backtrack",
        prompt="overwrite file `foo/../bar` with text `hello`",
        before=[DirEntry("foo"), FileEntry("bar", "old")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="overwrite_file_parent_reentry",
        prompt="overwrite file `../project/foo` with text `hello`",
        before=[FileEntry("foo", "old")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="overwrite_file_parent_backtrack",
        prompt="overwrite file `../foo/../bar` with text `hello`",
        before=[DirEntry("../foo"), FileEntry("../bar", "old")],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello")],
    ),
]

# Clear: project, parent, symlink file, symlink dir file, spaces
FILE_CLEAR_TASKS: list[Task] = [
    Task(
        name="clear_project_file",
        prompt="clear the content of file `foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "")],
    ),
    Task(
        name="clear_parent_file",
        prompt="clear the content of file `../foo`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../foo", "")],
    ),
    Task(
        name="clear_symlink_file",
        prompt="clear the content of file `foo`",
        before=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", ""), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="clear_symlink_dir_file",
        prompt="clear the content of file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", ""),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="clear_file_with_spaces",
        prompt="clear the content of file `foo bar`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("foo bar", ""),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="clear_file_subdir_backtrack",
        prompt="clear the content of file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("bar", "")],
    ),
    Task(
        name="clear_file_parent_reentry",
        prompt="clear the content of file `../project/foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "")],
    ),
    Task(
        name="clear_file_parent_backtrack",
        prompt="clear the content of file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        after=[DirEntry("../foo"), FileEntry("../bar", "")],
    ),
]

# Edit: project, parent, symlink file, symlink dir file, spaces
FILE_EDIT_TASKS: list[Task] = [
    Task(
        name="edit_project_file",
        prompt="replace text `hello` with `replaced` in file `foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "replaced")],
    ),
    Task(
        name="edit_parent_file",
        prompt="replace text `hello` with `replaced` in file `../foo`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../foo", "replaced")],
    ),
    Task(
        name="edit_symlink_file",
        prompt="replace text `hello` with `replaced` in file `foo`",
        before=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "replaced"), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="edit_symlink_dir_file",
        prompt="replace text `hello` with `replaced` in file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "replaced"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="edit_file_with_spaces",
        prompt="replace text `hello` with `replaced` in file `foo bar`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("foo bar", "replaced"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="edit_file_subdir_backtrack",
        prompt="replace text `hello` with `replaced` in file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("bar", "replaced")],
    ),
    Task(
        name="edit_file_parent_reentry",
        prompt="replace text `hello` with `replaced` in file `../project/foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "replaced")],
    ),
    Task(
        name="edit_file_parent_backtrack",
        prompt="replace text `hello` with `replaced` in file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        after=[DirEntry("../foo"), FileEntry("../bar", "replaced")],
    ),
]

# File Create: project, parent, symlink dir, spaces
FILE_CREATE_TASKS: list[Task] = [
    Task(
        name="create_project_file",
        prompt="create a new file `foo.txt`",
        after=[FileEntry("foo.txt")],
    ),
    Task(
        name="create_parent_file",
        prompt="create a new file `../foo.txt`",
        after=[FileEntry("../foo.txt")],
    ),
    Task(
        name="create_symlink_dir_file",
        prompt="create a new file `baz/foo.txt`",
        before=[DirEntry("../baz"), SymlinkEntry("baz", "../baz")],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo.txt"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="create_file_with_spaces",
        prompt="create a new file `foo bar` with content `hello`",
        before=[FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="create_file_subdir_backtrack",
        prompt="create a new file `foo/../bar` with content `hello`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="create_file_parent_reentry",
        prompt="create a new file `../project/foo` with content `hello`",
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="create_file_parent_backtrack",
        prompt="create a new file `../foo/../bar` with content `hello`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello")],
    ),
]

# Dir Create: project, parent, spaces, missing parent
DIR_CREATE_TASKS: list[Task] = [
    Task(
        name="create_project_dir",
        prompt="create a new directory `foo`",
        after=[DirEntry("foo")],
    ),
    Task(
        name="create_parent_dir",
        prompt="create a new directory `../foo`",
        after=[DirEntry("../foo")],
    ),
    Task(
        name="create_dir_with_spaces",
        prompt="create a new directory `foo bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="create_file_in_missing_dir",
        prompt="create a new file `bar/foo` with content `hello`",
        after=[DirEntry("bar"), FileEntry("bar/foo", "hello")],
    ),
    Task(
        name="create_dir_in_missing_dir",
        prompt="create a new directory `bar/foo`",
        after=[DirEntry("bar"), DirEntry("bar/foo")],
    ),
    Task(
        name="create_dir_subdir_backtrack",
        prompt="create a new directory `foo/../bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="create_dir_parent_reentry",
        prompt="create a new directory `../project/foo`",
        after=[DirEntry("foo")],
    ),
    Task(
        name="create_dir_parent_backtrack",
        prompt="create a new directory `../foo/../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), DirEntry("../bar")],
    ),
]

# File Delete: project, parent, symlink file, symlink dir file, spaces
FILE_DELETE_TASKS: list[Task] = [
    Task(
        name="delete_project_file",
        prompt="delete file `foo`",
        before=[FileEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_parent_file",
        prompt="delete file `../foo`",
        before=[FileEntry("../foo")],
        after=[],
    ),
    Task(
        name="delete_symlink_file",
        prompt="delete file `foo`",
        before=[FileEntry("../foo"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo")],
    ),
    Task(
        name="delete_symlink_dir_file",
        prompt="delete file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[DirEntry("../baz"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="delete_file_with_spaces",
        prompt="delete file `foo bar`",
        before=[FileEntry("foo bar"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    Task(
        name="delete_file_subdir_backtrack",
        prompt="delete file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar")],
        after=[DirEntry("foo")],
    ),
    Task(
        name="delete_file_parent_reentry",
        prompt="delete file `../project/foo`",
        before=[FileEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_file_parent_backtrack",
        prompt="delete file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar")],
        after=[DirEntry("../foo")],
    ),
]

# Dir Delete: project, parent, spaces, non-empty
DIR_DELETE_TASKS: list[Task] = [
    Task(
        name="delete_project_dir",
        prompt="delete directory `foo`",
        before=[DirEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_parent_dir",
        prompt="delete directory `../foo`",
        before=[DirEntry("../foo")],
        after=[],
    ),
    Task(
        name="delete_dir_with_spaces",
        prompt="delete directory `foo bar`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="delete_nonempty_dir",
        prompt="delete directory `foo` and all its contents",
        before=[FileEntry("foo/bar")],
        after=[],
    ),
    Task(
        name="delete_dir_subdir_backtrack",
        prompt="delete directory `foo/../bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo")],
    ),
    Task(
        name="delete_dir_parent_reentry",
        prompt="delete directory `../project/foo`",
        before=[DirEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_dir_parent_backtrack",
        prompt="delete directory `../foo/../bar`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo")],
    ),
]

# File Rename: project, parent, symlink file, symlink dir file, spaces
FILE_RENAME_TASKS: list[Task] = [
    Task(
        name="rename_project_file",
        prompt="rename file `foo` to `bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("bar", "hello")],
    ),
    Task(
        name="rename_parent_file",
        prompt="rename file `../foo` to `../bar`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../bar", "hello")],
    ),
    Task(
        name="rename_symlink_file",
        prompt="rename file `foo` to `bar`",
        before=[FileEntry("../foo"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo"), SymlinkEntry("bar", "../foo")],
    ),
    Task(
        name="rename_symlink_dir_file",
        prompt="rename file `baz/foo` to `baz/bar`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/bar"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="rename_file_with_spaces",
        prompt="rename file `foo bar` to `bar baz`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("bar baz", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="rename_file_subdir_backtrack",
        prompt="rename file `foo/../bar` to `baz`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("baz", "hello")],
    ),
    Task(
        name="rename_file_parent_reentry",
        prompt="rename file `../project/foo` to `../project/bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("bar", "hello")],
    ),
    Task(
        name="rename_file_parent_backtrack",
        prompt="rename file `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        after=[DirEntry("../foo"), FileEntry("../baz", "hello")],
    ),
]

# Dir Rename: project, parent, spaces, non-empty
DIR_RENAME_TASKS: list[Task] = [
    Task(
        name="rename_project_dir",
        prompt="rename directory `foo` to `bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("bar")],
    ),
    Task(
        name="rename_parent_dir",
        prompt="rename directory `../foo` to `../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../bar")],
    ),
    Task(
        name="rename_dir_with_spaces",
        prompt="rename directory `foo bar` to `bar baz`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("bar baz"), DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="rename_nonempty_dir",
        prompt="rename directory `foo` to `bar`",
        before=[FileEntry("foo/a.txt", "A"), FileEntry("foo/sub/b.txt", "B")],
        after=[FileEntry("bar/a.txt", "A"), FileEntry("bar/sub/b.txt", "B")],
    ),
    Task(
        name="rename_dir_subdir_backtrack",
        prompt="rename directory `foo/../bar` to `baz`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("baz")],
    ),
    Task(
        name="rename_dir_parent_reentry",
        prompt="rename directory `../project/foo` to `../project/bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("bar")],
    ),
    Task(
        name="rename_dir_parent_backtrack",
        prompt="rename directory `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo"), DirEntry("../baz")],
    ),
]

# File Copy: project, parent, symlink file, symlink dir file, spaces
FILE_COPY_TASKS: list[Task] = [
    Task(
        name="copy_project_file",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "hello"), FileEntry("bar", "hello")],
    ),
    Task(
        name="copy_parent_file",
        prompt="copy file `../foo` to `../bar`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../foo", "hello"), FileEntry("../bar", "hello")],
    ),
    Task(
        name="copy_symlink_file",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
        after=[
            FileEntry("../foo", "hello"),
            SymlinkEntry("foo", "../foo"),
            FileEntry("bar", "hello"),
        ],
    ),
    Task(
        name="copy_symlink_dir_file",
        prompt="copy file `baz/foo` to `baz/bar`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello"),
            FileEntry("../baz/bar", "hello"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="copy_file_with_spaces",
        prompt="copy file `foo bar` to `bar baz`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            FileEntry("foo bar", "hello"),
            FileEntry("bar baz", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="copy_file_subdir_backtrack",
        prompt="copy file `foo/../bar` to `baz`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("bar", "hello"), FileEntry("baz", "hello")],
    ),
    Task(
        name="copy_file_parent_reentry",
        prompt="copy file `../project/foo` to `../project/bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "hello"), FileEntry("bar", "hello")],
    ),
    Task(
        name="copy_file_parent_backtrack",
        prompt="copy file `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello"), FileEntry("../baz", "hello")],
    ),
]

# Dir Copy: project, parent, spaces, non-empty
DIR_COPY_TASKS: list[Task] = [
    Task(
        name="copy_project_dir",
        prompt="copy directory `foo` to `bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="copy_parent_dir",
        prompt="copy directory `../foo` to `../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), DirEntry("../bar")],
    ),
    Task(
        name="copy_dir_with_spaces",
        prompt="copy directory `foo bar` to `bar baz`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[
            DirEntry("foo bar"),
            DirEntry("bar baz"),
            DirEntry("foo"),
            DirEntry("bar"),
        ],
    ),
    Task(
        name="copy_nonempty_dir",
        prompt="copy directory `foo` to `bar` including all files and subdirectories",
        before=[FileEntry("foo/a.txt", "A"), FileEntry("foo/sub/b.txt", "B")],
        after=[
            FileEntry("foo/a.txt", "A"),
            FileEntry("foo/sub/b.txt", "B"),
            FileEntry("bar/a.txt", "A"),
            FileEntry("bar/sub/b.txt", "B"),
        ],
    ),
    Task(
        name="copy_dir_subdir_backtrack",
        prompt="copy directory `foo/../bar` to `baz`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("bar"), DirEntry("baz")],
    ),
    Task(
        name="copy_dir_parent_reentry",
        prompt="copy directory `../project/foo` to `../project/bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="copy_dir_parent_backtrack",
        prompt="copy directory `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo"), DirEntry("../bar"), DirEntry("../baz")],
    ),
]

# Chmod: project, parent, symlink file, symlink dir file, spaces
CHMOD_TASKS: list[Task] = [
    Task(
        name="chmod_project_file",
        prompt="change permissions of file `foo` to `600`",
        before=[FileEntry("foo", "hello", 0o644)],
        after=[FileEntry("foo", "hello", 0o600)],
    ),
    Task(
        name="chmod_parent_file",
        prompt="change permissions of file `../foo` to `600`",
        before=[FileEntry("../foo", "hello", 0o644)],
        after=[FileEntry("../foo", "hello", 0o600)],
    ),
    Task(
        name="chmod_symlink_file",
        prompt="change permissions of file `foo` to `600`",
        before=[FileEntry("../foo", "hello", 0o644), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "hello", 0o600), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="chmod_symlink_dir_file",
        prompt="change permissions of file `baz/foo` to `600`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello", 0o644),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "hello", 0o600),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="chmod_file_with_spaces",
        prompt="change permissions of file `foo bar` to `600`",
        before=[
            FileEntry("foo bar", "hello", 0o644),
            FileEntry("foo", "abc", 0o644),
            FileEntry("bar", "def", 0o644),
        ],
        after=[
            FileEntry("foo bar", "hello", 0o600),
            FileEntry("foo", "abc", 0o644),
            FileEntry("bar", "def", 0o644),
        ],
    ),
    Task(
        name="chmod_file_subdir_backtrack",
        prompt="change permissions of file `foo/../bar` to `600`",
        before=[DirEntry("foo"), FileEntry("bar", "hello", 0o644)],
        after=[DirEntry("foo"), FileEntry("bar", "hello", 0o600)],
    ),
    Task(
        name="chmod_file_parent_reentry",
        prompt="change permissions of file `../project/foo` to `600`",
        before=[FileEntry("foo", "hello", 0o644)],
        after=[FileEntry("foo", "hello", 0o600)],
    ),
    Task(
        name="chmod_file_parent_backtrack",
        prompt="change permissions of file `../foo/../bar` to `600`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello", 0o644)],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello", 0o600)],
    ),
]

# Symlink Create: project, parent, symlink dir, spaces
SYMLINK_CREATE_TASKS: list[Task] = [
    Task(
        name="create_project_symlink",
        prompt="create a symlink `foo` pointing to `bar`",
        before=[FileEntry("bar", "hello")],
        after=[FileEntry("bar", "hello"), SymlinkEntry("foo", "bar")],
    ),
    Task(
        name="create_parent_symlink",
        prompt="create a symlink `../foo` pointing to `/`",
        before=[],
        after=[SymlinkEntry("../foo", "/")],
    ),
    Task(
        name="create_symlink_with_spaces",
        prompt="create a symlink `foo bar` pointing to `/`",
        before=[SymlinkEntry("foo", "/"), SymlinkEntry("bar", "/")],
        after=[
            SymlinkEntry("foo", "/"),
            SymlinkEntry("bar", "/"),
            SymlinkEntry("foo bar", "/"),
        ],
    ),
]

# Glob: project, parent, symlink dir, spaces
GLOB_TASKS: list[Task] = [
    Task(
        name="glob_project_dir",
        prompt="list all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "A"),
            FileEntry("b.txt", "B"),
            FileEntry("c.log", "C"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_parent_dir",
        prompt="list all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "A"),
            FileEntry("../b.txt", "B"),
            FileEntry("../c.log", "C"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_symlink_dir",
        prompt="list all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "A"),
            FileEntry("../baz/b.txt", "B"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_dir_with_spaces",
        prompt="list all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "A"),
            FileEntry("foo bar/b.txt", "B"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_subdir_backtrack",
        prompt="list all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_parent_reentry",
        prompt="list all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_parent_backtrack",
        prompt="list all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "A"), FileEntry("../b.txt", "B"), FileEntry("../c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
]

# Glob and Read: project, parent, symlink dir, spaces
GLOB_AND_READ_TASKS: list[Task] = [
    Task(
        name="glob_and_read_project_dir",
        prompt="read all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "hello"),
            FileEntry("b.txt", "world"),
            FileEntry("c.log", "ignore"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_parent_dir",
        prompt="read all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "hello"),
            FileEntry("../b.txt", "world"),
            FileEntry("../c.log", "ignore"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_symlink_dir",
        prompt="read all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "hello"),
            FileEntry("../baz/b.txt", "world"),
            FileEntry("../baz/c.log", "ignore"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_dir_with_spaces",
        prompt="read all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "hello"),
            FileEntry("foo bar/b.txt", "world"),
            FileEntry("foo bar/c.log", "ignore"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_subdir_backtrack",
        prompt="read all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "hello"), FileEntry("b.txt", "world"), FileEntry("c.log", "ignore")],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_parent_reentry",
        prompt="read all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "hello"), FileEntry("b.txt", "world"), FileEntry("c.log", "ignore")],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_parent_backtrack",
        prompt="read all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "hello"), FileEntry("../b.txt", "world"), FileEntry("../c.log", "ignore")],
        outputs=["hello", "world"],
    ),
]

# Glob and Delete: project, parent, symlink dir, spaces
GLOB_AND_DELETE_TASKS: list[Task] = [
    Task(
        name="glob_and_delete_project_dir",
        prompt="delete all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "A"),
            FileEntry("b.txt", "B"),
            FileEntry("c.log", "C"),
        ],
        after=[FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_dir",
        prompt="delete all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "A"),
            FileEntry("../b.txt", "B"),
            FileEntry("../c.log", "C"),
        ],
        after=[FileEntry("../c.log", "C")],
    ),
    Task(
        name="glob_and_delete_symlink_dir",
        prompt="delete all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "A"),
            FileEntry("../baz/b.txt", "B"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="glob_and_delete_dir_with_spaces",
        prompt="delete all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "A"),
            FileEntry("foo bar/b.txt", "B"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            DirEntry("foo bar"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="glob_and_delete_subdir_backtrack",
        prompt="delete all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        after=[DirEntry("foo"), FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_reentry",
        prompt="delete all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        after=[FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_backtrack",
        prompt="delete all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "A"), FileEntry("../b.txt", "B"), FileEntry("../c.log", "C")],
        after=[DirEntry("../foo"), FileEntry("../c.log", "C")],
    ),
]

# Grep: project, parent, symlink dir, spaces
GREP_TASKS: list[Task] = [
    Task(
        name="grep_project_dir",
        prompt="find all files containing the text `needle` in directory `.`",
        before=[
            FileEntry("a.txt", "contains needle here"),
            FileEntry("b.txt", "no match"),
            FileEntry("c.txt", "needle again"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_parent_dir",
        prompt="find all files containing the text `needle` in directory `..`",
        before=[
            FileEntry("../a.txt", "contains needle here"),
            FileEntry("../b.txt", "no match"),
            FileEntry("../c.txt", "needle again"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_symlink_dir",
        prompt="find all files containing the text `needle` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "contains needle here"),
            FileEntry("../baz/b.txt", "no match"),
            FileEntry("../baz/c.txt", "needle again"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_dir_with_spaces",
        prompt="find all files containing the text `needle` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "contains needle here"),
            FileEntry("foo bar/b.txt", "no match"),
            FileEntry("foo bar/c.txt", "needle again"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_subdir_backtrack",
        prompt="find all files containing the text `needle` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "contains needle here"), FileEntry("b.txt", "no match"), FileEntry("c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_parent_reentry",
        prompt="find all files containing the text `needle` in directory `../project`",
        before=[FileEntry("a.txt", "contains needle here"), FileEntry("b.txt", "no match"), FileEntry("c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_parent_backtrack",
        prompt="find all files containing the text `needle` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "contains needle here"), FileEntry("../b.txt", "no match"), FileEntry("../c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
]

TASKS: list[Task] = [
    *DIR_LIST_TASKS,
    *FILE_READ_TASKS,
    *FILE_APPEND_TASKS,
    *FILE_OVERWRITE_TASKS,
    *FILE_CLEAR_TASKS,
    *FILE_EDIT_TASKS,
    *FILE_CREATE_TASKS,
    *DIR_CREATE_TASKS,
    *FILE_DELETE_TASKS,
    *DIR_DELETE_TASKS,
    *FILE_RENAME_TASKS,
    *DIR_RENAME_TASKS,
    *FILE_COPY_TASKS,
    *DIR_COPY_TASKS,
    *CHMOD_TASKS,
    *SYMLINK_CREATE_TASKS,
    *GLOB_TASKS,
    *GLOB_AND_READ_TASKS,
    *GLOB_AND_DELETE_TASKS,
    *GREP_TASKS,
]

EDGE_CASES: list[Task] = [
    # Edge cases: hidden files
    Task(
        name="create_hidden_project_file",
        prompt="create a new file `.env` with content `TOKEN=abc`",
        after=[FileEntry(".env", "TOKEN=abc")],
    ),
    # Edge cases: target conflict
    Task(
        name="move_project_file_to_existing_target",
        prompt="move file `foo` to `bar`",
        before=[FileEntry("foo", "hello"), FileEntry("bar", "world")],
        after=[FileEntry("bar", "hello")],
    ),
    # Edge cases: multi-step
    Task(
        name="copy_project_file_then_overwrite_source",
        prompt="copy file `foo` to `bar` and then overwrite `foo` with text `updated`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "updated"), FileEntry("bar", "hello")],
    ),
]

ALL_TASKS: list[Task] = [*TASKS, *EDGE_CASES]
