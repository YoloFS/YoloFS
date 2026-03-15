from microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# File Move: project, parent, symlink file, symlink dir file, spaces
MOVE_FILE_TASKS: list[Task] = [
    Task(
        name="move_file_project",
        prompt="move file `foo` to `bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("bar", "hello")],
    ),
    Task(
        name="move_file_parent",
        prompt="move file `../foo` to `../bar`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../bar", "hello")],
    ),
    Task(
        name="move_file_symlink",
        prompt="move file `foo` to `bar`",
        before=[FileEntry("../foo"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo"), SymlinkEntry("bar", "../foo")],
    ),
    Task(
        name="move_file_symlink_dir",
        prompt="move file `baz/foo` to `baz/bar`",
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
        name="move_file_with_spaces",
        prompt="move file `foo bar` to `bar baz`",
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
        name="move_file_project_backtrack",
        prompt="move file `foo/../bar` to `baz`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("baz", "hello")],
    ),
    Task(
        name="move_file_project_reentry",
        prompt="move file `../project/foo` to `../project/bar`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("bar", "hello")],
    ),
    Task(
        name="move_file_parent_backtrack",
        prompt="move file `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        after=[DirEntry("../foo"), FileEntry("../baz", "hello")],
    ),
    Task(
        name="move_file_to_existing_file",
        prompt="move file `foo` to `bar`",
        before=[FileEntry("foo", "hello"), FileEntry("bar", "world")],
        after=[FileEntry("bar", "hello")],
    ),
    Task(
        name="move_file_to_existing_dir",
        prompt="move file `foo` to `bar`",
        before=[FileEntry("foo", "hello"), DirEntry("bar")],
        after=[DirEntry("bar"), FileEntry("bar/foo", "hello")],
    ),
]
