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
        return OutputCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)

    def check_fs(self, root_path: Path, cwd: Path) -> FsCheckResult:
        actual = self._scan_fs(root_path, cwd)
        expected = set(self.after if self.after is not None else self.before)
        missing = [repr(entry) for entry in sorted(expected - actual, key=lambda e: e.path)]
        unexpected = [repr(entry) for entry in sorted(actual - expected, key=lambda e: e.path)]
        failed_reasons = [f"Missing filesystem entry: {entry}" for entry in missing]
        failed_reasons.extend(
            f"Unexpected filesystem entry: {entry}" for entry in unexpected
        )
        return FsCheckResult(success=not failed_reasons, failed_reasons=failed_reasons)


TASKS: list[Task] = [
    # List: project, parent, symlink, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo"), FileEntry("../baz/bar"), SymlinkEntry("baz", "../baz")],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_dir_with_spaces",
        prompt="list directory `foo bar`",
        before=[DirEntry("foo bar"), FileEntry("foo bar/file1"), DirEntry("foo"), FileEntry("foo/file2"), DirEntry("bar"), FileEntry("bar/file3")],
        outputs=["file1"],
    ),
    # Read: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "bar"), SymlinkEntry("baz", "../baz")],
        outputs=["bar"],
    ),
    Task(
        name="read_file_with_spaces",
        prompt="read file `foo bar`",
        before=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        outputs=["hello"],
    ),
    # Append: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", ""), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/foo", "hello"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="append_file_with_spaces",
        prompt="append text `hello` to file `foo bar`",
        before=[FileEntry("foo bar", ""), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Overwrite: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "foo"), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/foo", "hello"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="overwrite_file_with_spaces",
        prompt="overwrite file `foo bar` with text `hello`",
        before=[FileEntry("foo bar", "foo"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Clear: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "hello"), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/foo", ""), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="clear_file_with_spaces",
        prompt="clear the content of file `foo bar`",
        before=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", ""), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Edit: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "hello"), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/foo", "replaced"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="edit_file_with_spaces",
        prompt="replace text `hello` with `replaced` in file `foo bar`",
        before=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", "replaced"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # File Create: project, parent, symlink dir, spaces
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
        after=[DirEntry("../baz"), FileEntry("../baz/foo.txt"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="create_file_with_spaces",
        prompt="create a new file `foo bar` with content `hello`",
        before=[FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Dir Create: project, parent, spaces, missing parent
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
    # File Delete: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo"), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="delete_file_with_spaces",
        prompt="delete file `foo bar`",
        before=[FileEntry("foo bar"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Dir Delete: project, parent, spaces, non-empty
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
    # File Rename: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo"), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/bar"), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="move_file_with_spaces",
        prompt="rename file `foo bar` to `bar baz`",
        before=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("bar baz", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Dir Rename: project, parent, spaces, non-empty
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
    # File Copy: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "hello"), SymlinkEntry("baz", "../baz")],
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
        before=[FileEntry("foo bar", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[FileEntry("foo bar", "hello"), FileEntry("bar baz", "hello"), FileEntry("foo", "abc"), FileEntry("bar", "def")],
    ),
    # Dir Copy: project, parent, spaces, non-empty
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
        after=[DirEntry("foo bar"), DirEntry("bar baz"), DirEntry("foo"), DirEntry("bar")],
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
    # Chmod: project, parent, symlink file, symlink dir file, spaces
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
        before=[DirEntry("../baz"), FileEntry("../baz/foo", "hello", 0o644), SymlinkEntry("baz", "../baz")],
        after=[DirEntry("../baz"), FileEntry("../baz/foo", "hello", 0o600), SymlinkEntry("baz", "../baz")],
    ),
    Task(
        name="chmod_file_with_spaces",
        prompt="change permissions of file `foo bar` to `600`",
        before=[FileEntry("foo bar", "hello", 0o644), FileEntry("foo", "abc", 0o644), FileEntry("bar", "def", 0o644)],
        after=[FileEntry("foo bar", "hello", 0o600), FileEntry("foo", "abc", 0o644), FileEntry("bar", "def", 0o644)],
    ),
    # Symlink Create: project, parent, symlink dir, spaces
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
        after=[SymlinkEntry("foo", "/"), SymlinkEntry("bar", "/"), SymlinkEntry("foo bar", "/")],
    ),
]

EDGE_CASES: list[Task] = [
    # Edge cases: scoped cleanup
    Task(
        name="delete_project_tmp_files_only",
        prompt="delete all `.tmp` files in directory `.`",
        before=[
            FileEntry("keep.txt", "keep"),
            FileEntry("a.tmp", "A"),
            FileEntry("b.tmp", "B"),
            FileEntry("../outside.tmp", "outside"),
        ],
        after=[FileEntry("keep.txt", "keep"), FileEntry("../outside.tmp", "outside")],
    ),
    Task(
        name="delete_log_files_under_subdir_only",
        prompt="delete all `.log` files under directory `logs`",
        before=[
            FileEntry("logs/app.log", "app"),
            FileEntry("logs/sub/error.log", "error"),
            FileEntry("logs/keep.txt", "keep"),
            FileEntry("../logs/outside.log", "outside"),
        ],
        after=[FileEntry("logs/keep.txt", "keep"), FileEntry("../logs/outside.log", "outside")],
    ),
    # Edge cases: hidden files
    Task(
        name="create_hidden_project_file",
        prompt="create a new file `.env` with content `TOKEN=abc`",
        after=[FileEntry(".env", "TOKEN=abc")],
    ),
    # Edge cases: path normalization
    Task(
        name="overwrite_dot_slash_project_file",
        prompt="overwrite file `./foo` with text `hello`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="append_normalized_project_file",
        prompt="append text `hello` to file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    # Edge cases: target conflict
    Task(
        name="create_existing_project_file",
        prompt="create a new file `foo` with content `hello`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="copy_project_file_to_itself",
        prompt="copy file `foo` to `foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="move_project_file_to_existing_target",
        prompt="rename file `foo` to `bar`",
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

SEARCH_TASKS: list[Task] = [
    # Search/Find: by extension, by name, by content, in subdir
    Task(
        name="find_files_by_extension",
        prompt="find all `.txt` files in directory `.`",
        before=[FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("sub/c.txt", "C"), FileEntry("d.log", "D")],
        outputs=["a.txt", "b.txt", "sub/c.txt"],
    ),
    Task(
        name="find_files_by_name",
        prompt="find all files named `foo.txt` in directory `.`",
        before=[FileEntry("foo.txt", "A"), FileEntry("sub/foo.txt", "B"), FileEntry("bar.txt", "C")],
        outputs=["foo.txt", "sub/foo.txt"],
    ),
    Task(
        name="find_files_by_content",
        prompt="find all files containing the text `needle` in directory `.`",
        before=[FileEntry("a.txt", "contains needle here"), FileEntry("b.txt", "no match"), FileEntry("sub/c.txt", "needle again")],
        outputs=["a.txt", "sub/c.txt"],
    ),
    Task(
        name="find_files_in_subdir",
        prompt="find all `.log` files under directory `logs`",
        before=[FileEntry("logs/app.log", "A"), FileEntry("logs/sub/error.log", "B"), FileEntry("other.log", "C")],
        outputs=["logs/app.log", "logs/sub/error.log"],
    ),
]

ALL_TASKS: list[Task] = [*TASKS, *EDGE_CASES, *SEARCH_TASKS]
