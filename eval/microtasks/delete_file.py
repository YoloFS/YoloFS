from scripts.microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# File Delete: project, parent, symlink file, symlink dir file, spaces
DELETE_FILE_TASKS: list[Task] = [
    Task(
        name="delete_file_project",
        prompt="delete file `foo`",
        before=[FileEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_file_parent",
        prompt="delete file `../foo`",
        before=[FileEntry("../foo")],
        after=[],
    ),
    Task(
        name="delete_file_symlink",
        prompt="delete file `foo`",
        before=[FileEntry("../foo"), SymlinkEntry("foo", "../foo")],
        after=[FileEntry("../foo")],
    ),
    Task(
        name="delete_file_symlink_dir",
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
        name="delete_file_project_backtrack",
        prompt="delete file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar")],
        after=[DirEntry("foo")],
    ),
    Task(
        name="delete_file_project_reentry",
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
