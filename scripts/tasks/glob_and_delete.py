from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Glob and Delete: project, parent, symlink dir, spaces
GLOB_AND_DELETE_TASKS: list[Task] = [
    Task(
        name="glob_and_delete_project_dir",
        prompt="delete all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "A"),
            FileEntry("b.txt", "B"),
            FileEntry("c.log", "C"),
        ],
        after=[FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_dir",
        prompt="delete all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "A"),
            FileEntry("../b.txt", "B"),
            FileEntry("../c.log", "C"),
        ],
        after=[FileEntry("../c.log", "C")],
    ),
    Task(
        name="glob_and_delete_symlink_dir",
        prompt="delete all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "A"),
            FileEntry("../baz/b.txt", "B"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
        after=[
            DirEntry("../baz"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
    ),
    Task(
        name="glob_and_delete_dir_with_spaces",
        prompt="delete all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "A"),
            FileEntry("foo bar/b.txt", "B"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        after=[
            DirEntry("foo bar"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
    ),
    Task(
        name="glob_and_delete_subdir_backtrack",
        prompt="delete all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        after=[DirEntry("foo"), FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_reentry",
        prompt="delete all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        after=[FileEntry("c.log", "C")],
    ),
    Task(
        name="glob_and_delete_parent_backtrack",
        prompt="delete all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "A"), FileEntry("../b.txt", "B"), FileEntry("../c.log", "C")],
        after=[DirEntry("../foo"), FileEntry("../c.log", "C")],
    ),
]
