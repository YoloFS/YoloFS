from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# File Create: project, parent, symlink dir, spaces
CREATE_FILE_TASKS: list[Task] = [
    Task(
        name="create_file_project",
        prompt="create a new file `foo.txt`",
        after=[FileEntry("foo.txt")],
    ),
    Task(
        name="create_file_parent",
        prompt="create a new file `../foo.txt`",
        after=[FileEntry("../foo.txt")],
    ),
    Task(
        name="create_file_symlink_dir",
        prompt="create a new file `baz/foo.txt`",
        before=[DirEntry("../baz"), SymlinkEntry("baz", "../baz")],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/foo.txt"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="create_file_with_spaces",
        prompt="create a new file `foo bar` with content `hello`",
        before=[FileEntry("foo", "abc"), FileEntry("bar", "def")],
        after=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="create_file_project_backtrack",
        prompt="create a new file `foo/../bar` with content `hello`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), FileEntry("bar", "hello")],
    ),
    Task(
        name="create_file_project_reentry",
        prompt="create a new file `../project/foo` with content `hello`",
        after=[FileEntry("foo", "hello")],
    ),
    Task(
        name="create_file_parent_backtrack",
        prompt="create a new file `../foo/../bar` with content `hello`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), FileEntry("../bar", "hello")],
    ),
]
