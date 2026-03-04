from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Read: project, parent, symlink file, symlink dir file, spaces
FILE_READ_TASKS: list[Task] = [
    Task(
        name="read_project_file",
        prompt="read file `foo`",
        before=[FileEntry("foo", "bar")],
        outputs=["bar"],
    ),
    Task(
        name="read_parent_file",
        prompt="read file `../foo`",
        before=[FileEntry("../foo", "bar")],
        outputs=["bar"],
    ),
    Task(
        name="read_symlink_file",
        prompt="read file `foo`",
        before=[FileEntry("../foo", "bar"), SymlinkEntry("foo", "../foo")],
        outputs=["bar"],
    ),
    Task(
        name="read_symlink_dir_file",
        prompt="read file `baz/foo`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo", "bar"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["bar"],
    ),
    Task(
        name="read_file_with_spaces",
        prompt="read file `foo bar`",
        before=[
            FileEntry("foo bar", "hello"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["hello"],
    ),
    Task(
        name="read_file_subdir_backtrack",
        prompt="read file `foo/../bar`",
        before=[DirEntry("foo"), FileEntry("bar", "hello")],
        outputs=["hello"],
    ),
    Task(
        name="read_file_parent_reentry",
        prompt="read file `../project/foo`",
        before=[FileEntry("foo", "hello")],
        outputs=["hello"],
    ),
    Task(
        name="read_file_parent_backtrack",
        prompt="read file `../foo/../bar`",
        before=[DirEntry("../foo"), FileEntry("../bar", "hello")],
        outputs=["hello"],
    ),
]
