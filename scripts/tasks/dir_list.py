from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# List: project, parent, symlink, spaces
DIR_LIST_TASKS: list[Task] = [
    Task(
        name="list_project_dir",
        prompt="list directory `.`",
        before=[FileEntry("foo"), FileEntry("bar")],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_parent_dir",
        prompt="list directory `..`",
        before=[FileEntry("../foo"), FileEntry("../bar")],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_symlink_dir",
        prompt="list directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/foo"),
            FileEntry("../baz/bar"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["foo", "bar"],
    ),
    Task(
        name="list_dir_with_spaces",
        prompt="list directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/file1"),
            DirEntry("foo"),
            FileEntry("foo/file2"),
            DirEntry("bar"),
            FileEntry("bar/file3"),
        ],
        outputs=["file1"],
    ),
    Task(
        name="list_dir_project_backtrack",
        prompt="list directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("bar"), FileEntry("baz")],
        outputs=["bar", "baz"],
    ),
    Task(
        name="list_dir_project_reentry",
        prompt="list directory `../project`",
        before=[FileEntry("bar"), FileEntry("baz")],
        outputs=["bar", "baz"],
    ),
    Task(
        name="list_dir_parent_backtrack",
        prompt="list directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../bar"), FileEntry("../baz")],
        outputs=["bar", "baz"],
    ),
]
