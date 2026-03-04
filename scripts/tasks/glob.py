from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Glob: project, parent, symlink dir, spaces
GLOB_TASKS: list[Task] = [
    Task(
        name="glob_project_dir",
        prompt="list all files matching `*.txt` in directory `.`",
        before=[
            FileEntry("a.txt", "A"),
            FileEntry("b.txt", "B"),
            FileEntry("c.log", "C"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_parent_dir",
        prompt="list all files matching `*.txt` in directory `..`",
        before=[
            FileEntry("../a.txt", "A"),
            FileEntry("../b.txt", "B"),
            FileEntry("../c.log", "C"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_symlink_dir",
        prompt="list all files matching `*.txt` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "A"),
            FileEntry("../baz/b.txt", "B"),
            FileEntry("../baz/c.log", "C"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_dir_with_spaces",
        prompt="list all files matching `*.txt` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "A"),
            FileEntry("foo bar/b.txt", "B"),
            FileEntry("foo bar/c.log", "C"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_project_backtrack",
        prompt="list all files matching `*.txt` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_project_reentry",
        prompt="list all files matching `*.txt` in directory `../project`",
        before=[FileEntry("a.txt", "A"), FileEntry("b.txt", "B"), FileEntry("c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
    Task(
        name="glob_parent_backtrack",
        prompt="list all files matching `*.txt` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "A"), FileEntry("../b.txt", "B"), FileEntry("../c.log", "C")],
        outputs=["a.txt", "b.txt"],
    ),
]
