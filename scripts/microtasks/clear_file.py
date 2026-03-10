from scripts.microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Clear: project, parent, symlink file, symlink dir file, spaces
CLEAR_FILE_TASKS: list[Task] = [
    Task(
        name="clear_file_project",
        prompt="clear the content of file `foo`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "")],
    ),
    Task(
        name="clear_file_parent",
        prompt="clear the content of file `../foo`",
        before=[FileEntry("../foo", "hello")],
        after=[FileEntry("../foo", "")],
    ),
    Task(
        name="clear_file_symlink",
        prompt="clear the content of file `foo`",
        before=[FileEntry("../foo", "hello"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo", ""), SymlinkEntry("foo", "../foo")],
    ),
    Task(
        name="clear_file_symlink_dir",
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
        name="clear_file_project_backtrack",
        prompt="clear the content of file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        after=[DirEntry("foo"), FileEntry("bar", "")],
    ),
    Task(
        name="clear_file_project_reentry",
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
