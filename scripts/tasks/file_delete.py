from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# File Delete: project, parent, symlink file, symlink dir file, spaces
FILE_DELETE_TASKS: list[Task] = [
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
