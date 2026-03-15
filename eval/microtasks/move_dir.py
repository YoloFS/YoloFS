from scripts.microtasks.base import DirEntry, FileEntry, Task

# Dir Move: project, parent, spaces, non-empty
MOVE_DIR_TASKS: list[Task] = [
    Task(
        name="move_dir_project",
        prompt="move directory `foo` to `bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("bar")],
    ),
    Task(
        name="move_dir_parent",
        prompt="move directory `../foo` to `../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../bar")],
    ),
    Task(
        name="move_dir_with_spaces",
        prompt="move directory `foo bar` to `bar baz`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("bar baz"), DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="move_dir_nonempty",
        prompt="move directory `foo` to `bar`",
        before=[FileEntry("foo/a.txt", "A"), FileEntry("foo/sub/b.txt", "B")],
        after=[
            DirEntry("bar"),
            DirEntry("bar/sub"),
            FileEntry("bar/a.txt", "A"),
            FileEntry("bar/sub/b.txt", "B"),
        ],
    ),
    Task(
        name="move_dir_project_backtrack",
        prompt="move directory `foo/../bar` to `baz`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("baz")],
    ),
    Task(
        name="move_dir_project_reentry",
        prompt="move directory `../project/foo` to `../project/bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("bar")],
    ),
    Task(
        name="move_dir_parent_backtrack",
        prompt="move directory `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo"), DirEntry("../baz")],
    ),
    Task(
        name="move_dir_to_existing_dir",
        prompt="move directory `foo` to `bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("bar"), DirEntry("bar/foo")],
    ),
]
