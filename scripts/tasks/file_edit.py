from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

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
