from scripts.tasks.base import DirEntry, FileEntry, SymlinkEntry, Task

# Grep: project, parent, symlink dir, spaces
GREP_TASKS: list[Task] = [
    Task(
        name="grep_project_dir",
        prompt="find all files containing the text `needle` in directory `.`",
        before=[
            FileEntry("a.txt", "contains needle here"),
            FileEntry("b.txt", "no match"),
            FileEntry("c.txt", "needle again"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_parent_dir",
        prompt="find all files containing the text `needle` in directory `..`",
        before=[
            FileEntry("../a.txt", "contains needle here"),
            FileEntry("../b.txt", "no match"),
            FileEntry("../c.txt", "needle again"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_symlink_dir",
        prompt="find all files containing the text `needle` in directory `baz`",
        before=[
            DirEntry("../baz"),
            FileEntry("../baz/a.txt", "contains needle here"),
            FileEntry("../baz/b.txt", "no match"),
            FileEntry("../baz/c.txt", "needle again"),
            SymlinkEntry("baz", "../baz"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_dir_with_spaces",
        prompt="find all files containing the text `needle` in directory `foo bar`",
        before=[
            DirEntry("foo bar"),
            FileEntry("foo bar/a.txt", "contains needle here"),
            FileEntry("foo bar/b.txt", "no match"),
            FileEntry("foo bar/c.txt", "needle again"),
            FileEntry("foo", "abc"),
            FileEntry("bar", "def"),
        ],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_project_backtrack",
        prompt="find all files containing the text `needle` in directory `foo/..`",
        before=[DirEntry("foo"), FileEntry("a.txt", "contains needle here"), FileEntry("b.txt", "no match"), FileEntry("c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_project_reentry",
        prompt="find all files containing the text `needle` in directory `../project`",
        before=[FileEntry("a.txt", "contains needle here"), FileEntry("b.txt", "no match"), FileEntry("c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
    Task(
        name="grep_parent_backtrack",
        prompt="find all files containing the text `needle` in directory `../foo/..`",
        before=[DirEntry("../foo"), FileEntry("../a.txt", "contains needle here"), FileEntry("../b.txt", "no match"), FileEntry("../c.txt", "needle again")],
        outputs=["a.txt", "c.txt"],
    ),
]
