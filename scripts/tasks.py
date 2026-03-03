import os
import stat
from dataclasses import dataclass, field
from pathlib import Path

from scripts.models import ToolCall


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

    def check_outputs(self, tool_calls: list[ToolCall]) -> bool:
        ok = True
        tc_outputs = [str(tc.output) for tc in tool_calls]
        for expected in self.outputs:
            found = any(expected in o for o in tc_outputs)
            print(f"  Output {expected!r}: {'found' if found else 'not found'}")
            ok = ok and found
        return ok

    def check_fs(self, root_path: Path, cwd: Path) -> bool:
        actual = self._scan_fs(root_path, cwd)
        expected = set(self.after if self.after is not None else self.before)

        if actual == expected:
            print("  Filesystem: matches")
        else:
            for entry in sorted(expected - actual, key=lambda e: e.path):
                print(f"  Missing: {entry}")
            for entry in sorted(actual - expected, key=lambda e: e.path):
                print(f"  Unexpected: {entry}")
        return actual == expected

    def check(self, root_path: Path, cwd: Path, tool_calls: list[ToolCall]) -> bool:
        return self.check_outputs(tool_calls) and self.check_fs(root_path, cwd)


TASKS: list[Task] = [
    # List
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
        prompt="list directory `dir`",
        before=[FileEntry("../dir/foo"), FileEntry("../dir/bar"), SymlinkEntry("dir", "../dir")],
        outputs=["foo", "bar"],
    ),
    # Read
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
        prompt="read file `dir/foo`",
        before=[DirEntry("../dir"), FileEntry("../dir/foo", "bar"), SymlinkEntry("dir", "../dir")],
        outputs=["bar"],
    ),
    # Append empty
    Task(
        name="append_project_file",
        prompt="append text `hello` to file `foo`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="append_parent_file",
        prompt="append text `hello` to file `../file`",
        before=[FileEntry("../file", "")],
        after=[FileEntry("../file", "hello")],
    ),
    Task(
        name="append_symlink_file",
        prompt="append text `hello` to file `file`",
        before=[FileEntry("../file", ""), SymlinkEntry("file", "../file")],
        after=[FileEntry("../file", "hello"), SymlinkEntry("file", "../file")],
    ),
    Task(
        name="append_symlink_dir_file",
        prompt="append text `hello` to file `dir/file`",
        before=[FileEntry("../dir/file", ""), SymlinkEntry("dir", "../dir")],
        after=[FileEntry("../dir/file", "hello"), SymlinkEntry("dir", "../dir")],
    ),
    # Overwrite
    Task(
        name="overwrite_project_file",
        prompt="overwrite file `file` with text `hello`",
        before=[FileEntry("file", "")],
        after=[FileEntry("file", "hello")],
    ),
    Task(
        name="overwrite_parent_file",
        prompt="overwrite file `../file` with text `hello`",
        before=[FileEntry("../file", "")],
        after=[FileEntry("../file", "hello")],
    ),
    Task(
        name="overwrite_symlink_file",
        prompt="overwrite file `file` with text `hello`",
        before=[FileEntry("../file", ""), SymlinkEntry("file", "../file")],
        after=[FileEntry("../file", "hello"), SymlinkEntry("file", "../file")],
    ),
    Task(
        name="overwrite_symlink_dir_file",
        prompt="overwrite file `dir/file` with text `hello`",
        before=[FileEntry("../dir/file", ""), SymlinkEntry("dir", "../dir")],
        after=[FileEntry("../dir/file", "hello"), SymlinkEntry("dir", "../dir")],
    ),
    # Edit
    Task(
        name="edit_project_file",
        prompt="replace text `hello` with `replaced` in file `file`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "replaced")],
    ),
    Task(
        name="edit_parent_file",
        prompt="replace text `hello` with `replaced` in file `../file`",
        before=[FileEntry("../file", "hello")],
        after=[FileEntry("../file", "replaced")],
    ),
    Task(
        name="edit_symlink_file",
        prompt="replace text `hello` with `replaced` in file `file`",
        before=[FileEntry("../file", "hello"), SymlinkEntry("file", "../file")],
        after=[FileEntry("../file", "replaced"), SymlinkEntry("file", "../file")],
    ),
    Task(
        name="edit_symlink_dir_file",
        prompt="replace text `hello` with `replaced` in file `dir/file`",
        before=[FileEntry("../dir/file", "hello"), SymlinkEntry("dir", "../dir")],
        after=[FileEntry("../dir/file", "replaced"), SymlinkEntry("dir", "../dir")],
    ),
    # Create
    Task(
        name="create_project_file",
        prompt="create a new file `newfile.txt`",
        after=[FileEntry("newfile.txt")],
    ),
    Task(
        name="create_parent_file",
        prompt="create a new file `../newfile.txt`",
        after=[FileEntry("../newfile.txt")],
    ),
    Task(
        name="create_symlink_dir_file",
        prompt="create a new file `dir/newfile.txt`",
        before=[DirEntry("../dir"), SymlinkEntry("dir", "../dir")],
        after=[FileEntry("../dir/newfile.txt"), SymlinkEntry("dir", "../dir")],
    ),
    # Delete
    Task(
        name="delete_project_file",
        prompt="delete file `file`",
        before=[FileEntry("file")],
        after=[],
    ),
    Task(
        name="delete_parent_file",
        prompt="delete file `../file`",
        before=[FileEntry("../file")],
        after=[],
    ),
    Task(
        name="delete_symlink_file",
        prompt="delete file `file`",
        before=[FileEntry("../file"), SymlinkEntry("file", "../file")],
        after=[FileEntry("../file")],
    ),
    Task(
        name="delete_symlink_dir_file",
        prompt="delete file `dir/file`",
        before=[FileEntry("../dir/file"), SymlinkEntry("dir", "../dir")],
        after=[SymlinkEntry("dir", "../dir")],
    ),
    # Rename
    Task(
        name="rename_project_file",
        prompt="rename file `file` to `file_renamed`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file_renamed", "hello")],
    ),
    Task(
        name="rename_parent_file",
        prompt="rename file `../file` to `../file_renamed`",
        before=[FileEntry("../file", "hello")],
        after=[FileEntry("../file_renamed", "hello")],
    ),
    Task(
        name="rename_symlink_file",
        prompt="rename file `file` to `file_renamed`",
        before=[FileEntry("../file"), SymlinkEntry("file", "../file")],
        after=[FileEntry("../file"), SymlinkEntry("file_renamed", "../file")],
    ),
    Task(
        name="rename_symlink_dir_file",
        prompt="rename file `dir/file` to `dir/file_renamed`",
        before=[FileEntry("../dir/file"), SymlinkEntry("dir", "../dir")],
        after=[FileEntry("../dir/file_renamed"), SymlinkEntry("dir", "../dir")],
    ),
    # Copy
    Task(
        name="copy_project_file",
        prompt="copy file `file` to `file_copy`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "hello"), FileEntry("file_copy", "hello")],
    ),
    Task(
        name="copy_parent_file",
        prompt="copy file `../file` to `../file_copy`",
        before=[FileEntry("../file", "hello")],
        after=[FileEntry("../file", "hello"), FileEntry("../file_copy", "hello")],
    ),
    Task(
        name="copy_symlink_file",
        prompt="copy file `file` to `file_copy`",
        before=[FileEntry("../file", "hello"), SymlinkEntry("file", "../file")],
        after=[
            FileEntry("../file", "hello"),
            SymlinkEntry("file", "../file"),
            FileEntry("file_copy", "hello"),
        ],
    ),
    Task(
        name="copy_symlink_dir_file",
        prompt="copy file `dir/file` to `dir/file_copy`",
        before=[FileEntry("../dir/file", "hello"), SymlinkEntry("dir", "../dir")],
        after=[
            FileEntry("../dir/file", "hello"),
            FileEntry("../dir/file_copy", "hello"),
            SymlinkEntry("dir", "../dir"),
        ],
    ),
]

EDGE_CASES: list[Task] = [
    # Edge cases: content
    Task(
        name="append_multiline_project_file",
        prompt="append text `hello\\nworld` to file `file`",
        before=[FileEntry("file", "")],
        after=[FileEntry("file", "hello\nworld")],
    ),
    Task(
        name="overwrite_project_file_empty",
        prompt="overwrite file `file` with text ``",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "")],
    ),
    # Edge cases: names with spaces
    Task(
        name="create_project_file_with_spaces",
        prompt="create a new file `new file.txt` with content `hello`",
        after=[FileEntry("new file.txt", "hello")],
    ),
    Task(
        name="move_project_file_with_spaces",
        prompt="rename file `file` to `renamed file.txt`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("renamed file.txt", "hello")],
    ),
    # Edge cases: edit search
    Task(
        name="edit_second_occurrence_project_file",
        prompt="replace the second occurrence of `old` with `replaced` in file `file`",
        before=[FileEntry("file", "old old")],
        after=[FileEntry("file", "old replaced")],
    ),
    Task(
        name="edit_text_not_found_project_file",
        prompt="replace text `does-not-exist` with `replaced` in file `file`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "hello")],
    ),
    # Edge cases: missing source
    Task(
        name="read_missing_project_file",
        prompt="read file `missing.txt`",
    ),
    Task(
        name="move_missing_project_file",
        prompt="rename file `missing.txt` to `renamed_missing.txt`",
        after=[],
    ),
    Task(
        name="delete_same_file_twice",
        prompt="delete file `file` and then delete file `file` again",
        before=[FileEntry("file")],
        after=[],
    ),
    # Edge cases: target is directory
    Task(
        name="read_directory_path",
        prompt="read file `dir`",
        before=[DirEntry("dir")],
    ),
    Task(
        name="append_to_directory_path",
        prompt="append text `hello` to file `dir`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    Task(
        name="overwrite_directory_path",
        prompt="overwrite file `dir` with text `hello`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    Task(
        name="edit_directory_path",
        prompt="replace text `hello` with `replaced` in file `dir`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    Task(
        name="delete_directory_path",
        prompt="delete file `dir`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    Task(
        name="rename_directory_path",
        prompt="rename file `dir` to `dir_renamed`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    Task(
        name="copy_directory_path",
        prompt="copy file `dir` to `dir_copy`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir")],
    ),
    # Edge cases: directory operations
    Task(
        name="list_missing_dir",
        prompt="list directory `missing_dir`",
    ),
    Task(
        name="create_file_in_missing_dir",
        prompt="create a new file `newdir/newfile.txt` with content `hello`",
        after=[FileEntry("newdir/newfile.txt", "hello")],
    ),
    Task(
        name="rename_dir",
        prompt="rename directory `dir` to `dir_renamed`",
        before=[DirEntry("dir")],
        after=[DirEntry("dir_renamed")],
    ),
    Task(
        name="delete_dir",
        prompt="delete directory `dir`",
        before=[DirEntry("dir")],
        after=[],
    ),
    Task(
        name="delete_nonempty_dir",
        prompt="delete directory `dir` and all its contents",
        before=[FileEntry("dir/file")],
        after=[],
    ),
    Task(
        name="delete_nonempty_dir_without_recursive",
        prompt="delete directory `dir`",
        before=[FileEntry("dir/file", "hello")],
        after=[FileEntry("dir/file", "hello")],
    ),
    Task(
        name="copy_nonempty_dir",
        prompt="copy directory `dir` to `dir_copy` including all files and subdirectories",
        before=[FileEntry("dir/a.txt", "A"), FileEntry("dir/sub/b.txt", "B")],
        after=[
            FileEntry("dir/a.txt", "A"),
            FileEntry("dir/sub/b.txt", "B"),
            FileEntry("dir_copy/a.txt", "A"),
            FileEntry("dir_copy/sub/b.txt", "B"),
        ],
    ),
    Task(
        name="rename_nonempty_dir",
        prompt="rename directory `dir` to `dir_renamed`",
        before=[FileEntry("dir/a.txt", "A"), FileEntry("dir/sub/b.txt", "B")],
        after=[FileEntry("dir_renamed/a.txt", "A"), FileEntry("dir_renamed/sub/b.txt", "B")],
    ),
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
        prompt="overwrite file `./file` with text `hello`",
        before=[FileEntry("file", "")],
        after=[FileEntry("file", "hello")],
    ),
    Task(
        name="append_normalized_project_file",
        prompt="append text `hello` to file `dir/../file`",
        before=[DirEntry("dir"), FileEntry("file", "")],
        after=[DirEntry("dir"), FileEntry("file", "hello")],
    ),
    # Edge cases: permissions
    Task(
        name="chmod_project_file_mode_600",
        prompt="change permissions of file `secret.txt` to `600`",
        before=[FileEntry("secret.txt", "token", 0o644)],
        after=[FileEntry("secret.txt", "token", 0o600)],
    ),
    Task(
        name="chmod_project_file_mode_700",
        prompt="change permissions of file `script.sh` to `700`",
        before=[FileEntry("script.sh", "#!/bin/sh\necho hi\n", 0o644)],
        after=[FileEntry("script.sh", "#!/bin/sh\necho hi\n", 0o700)],
    ),
    Task(
        name="chmod_project_txt_files_only",
        prompt="change permissions of all `.txt` files in directory `.` to `600`",
        before=[
            FileEntry("a.txt", "A", 0o644),
            FileEntry("b.txt", "B", 0o644),
            FileEntry("../outside.txt", "OUTSIDE", 0o644),
        ],
        after=[
            FileEntry("a.txt", "A", 0o600),
            FileEntry("b.txt", "B", 0o600),
            FileEntry("../outside.txt", "OUTSIDE", 0o644),
        ],
    ),
    Task(
        name="chmod_symlink_target_file_mode_600",
        prompt="change permissions of file `link.txt` to `600`",
        before=[
            FileEntry("../target.txt", "token", 0o644),
            SymlinkEntry("link.txt", "../target.txt"),
        ],
        after=[
            FileEntry("../target.txt", "token", 0o600),
            SymlinkEntry("link.txt", "../target.txt"),
        ],
    ),
    # Edge cases: target conflict
    Task(
        name="create_existing_project_file",
        prompt="create a new file `file` with content `hello`",
        before=[FileEntry("file", "")],
        after=[FileEntry("file", "hello")],
    ),
    Task(
        name="copy_project_file_to_itself",
        prompt="copy file `file` to `file`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "hello")],
    ),
    Task(
        name="move_project_file_to_existing_target",
        prompt="rename file `file` to `file_target`",
        before=[FileEntry("file", "hello"), FileEntry("file_target", "world")],
        after=[FileEntry("file_target", "hello")],
    ),
    # Edge cases: multi-step
    Task(
        name="copy_project_file_then_overwrite_source",
        prompt="copy file `file` to `file_backup` and then overwrite `file` with text `updated`",
        before=[FileEntry("file", "hello")],
        after=[FileEntry("file", "updated"), FileEntry("file_backup", "hello")],
    ),
]

ALL_TASKS: list[Task] = [*TASKS, *EDGE_CASES]
