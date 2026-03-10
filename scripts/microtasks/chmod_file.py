from scripts.microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Chmod: project, parent, symlink file, symlink dir file, spaces
CHMOD_FILE_TASKS: list[Task] = [
    Task(
        name="chmod_file_project",
        prompt="change permissions of file `foo` to `600`",
        before=[FileEntry("foo", "hello", 0o644)],
        after=[FileEntry("foo", "hello", 0o600)],
    ),
    Task(
        name="chmod_file_parent",
        prompt="change permissions of file `../foo` to `600`",
        before=[FileEntry("../foo", "hello", 0o644)],
        after=[FileEntry("../foo", "hello", 0o600)],
    ),
    Task(
        name="chmod_file_symlink",
        prompt="change permissions of file `foo` to `600`",
        before=[FileEntry("../foo", "hello", 0o644), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "hello", 0o600), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="chmod_file_symlink_dir",
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
        name="chmod_file_project_backtrack",
        prompt="change permissions of file `foo/../bar` to `600`",
        before=[DirEntry("foo"), FileEntry("bar", "hello", 0o644)],
        after=[DirEntry("foo"), FileEntry("bar", "hello", 0o600)],
    ),
    Task(
        name="chmod_file_project_reentry",
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
