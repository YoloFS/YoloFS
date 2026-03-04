from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

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
        name="overwrite_file_project_backtrack",
        prompt="overwrite file `foo/../bar` with text `hello`",
        before=[DirEntry("foo"), FileEntry("bar", "old")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="overwrite_file_project_reentry",
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
