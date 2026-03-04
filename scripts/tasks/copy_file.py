from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# File Copy: project, parent, symlink file, symlink dir file, spaces
COPY_FILE_TASKS: list[Task] = [
    Task(
        name="copy_file_project",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "hello"), FileEntry("bar", "hello")],
    ),
    Task(
        name="copy_file_parent",
        prompt="copy file `../foo` to `../bar`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../foo", "hello"), FileEntry("../bar", "hello")],
    ),
    Task(
        name="copy_file_symlink",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
        after=[
            FileEntry("../foo", "hello"),
            SymlinkEntry("foo", "../foo"),
            FileEntry("bar", "hello"),
        ],
    ),
    Task(
        name="copy_file_symlink_dir",
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
        name="copy_file_project_backtrack",
        prompt="copy file `foo/../bar` to `baz`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("bar", "hello"), FileEntry("baz", "hello")],
    ),
    Task(
        name="copy_file_project_reentry",
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
    Task(
        name="copy_file_to_existing_file",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("foo", "hello"), FileEntry("bar", "world")],
        after=[FileEntry("foo", "hello"), FileEntry("bar", "hello")],
    ),
    Task(
        name="copy_file_to_existing_dir",
        prompt="copy file `foo` to `bar`",
        before=[FileEntry("foo", "hello"), DirEntry("bar")],
        after=[FileEntry("foo", "hello"), DirEntry("bar"), FileEntry("bar/foo", "hello")],
    ),
]
