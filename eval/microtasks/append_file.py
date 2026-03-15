from scripts.microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Append: project, parent, symlink file, symlink dir file, spaces
APPEND_FILE_TASKS: list[Task] = [
    Task(
        name="append_file_project",
        prompt="append text `hello` to file `foo`",
        before=[FileEntry("foo", "")],
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="append_file_parent",
        prompt="append text `hello` to file `../foo`",
        before=[FileEntry("../foo", "")],
        after=[FileEntry("../foo", "hello")],
    ),
    Task(
        name="append_file_symlink",
        prompt="append text `hello` to file `foo`",
        before=[FileEntry("../foo", ""), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="append_file_symlink_dir",
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
        name="append_file_project_backtrack",
        prompt="append text `hello` to file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="append_file_project_reentry",
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
