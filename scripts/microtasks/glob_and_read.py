from scripts.microtasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Glob and Read: project, parent, symlink dir, spaces
GLOB_AND_READ_TASKS: list[Task] = [
    Task(
        name="glob_and_read_project_dir",
        prompt="read all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "hello"),
            FileEntry("b.txt", "world"),
            FileEntry("c.log", "ignore"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_parent_dir",
        prompt="read all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "hello"),
            FileEntry("../b.txt", "world"),
            FileEntry("../c.log", "ignore"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_symlink_dir",
        prompt="read all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "hello"),
            FileEntry("../baz/b.txt", "world"),
            FileEntry("../baz/c.log", "ignore"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_dir_with_spaces",
        prompt="read all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "hello"),
            FileEntry("foo bar/b.txt", "world"),
            FileEntry("foo bar/c.log", "ignore"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_project_backtrack",
        prompt="read all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "hello"), FileEntry("b.txt", "world"), FileEntry("c.log", "ignore")],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_project_reentry",
        prompt="read all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "hello"), FileEntry("b.txt", "world"), FileEntry("c.log", "ignore")],
        outputs=["hello", "world"],
    ),
    Task(
        name="glob_and_read_parent_backtrack",
        prompt="read all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "hello"), FileEntry("../b.txt", "world"), FileEntry("../c.log", "ignore")],
        outputs=["hello", "world"],
    ),
]
