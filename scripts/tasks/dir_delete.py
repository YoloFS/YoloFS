from scripts.tasks.base import DirEntry, FileEntry, Task

# Dir Delete: project, parent, spaces, non-empty
DIR_DELETE_TASKS: list[Task] = [
    Task(
        name="delete_project_dir",
        prompt="delete directory `foo`",
        before=[DirEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_parent_dir",
        prompt="delete directory `../foo`",
        before=[DirEntry("../foo")],
        after=[],
    ),
    Task(
        name="delete_dir_with_spaces",
        prompt="delete directory `foo bar`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="delete_nonempty_dir",
        prompt="delete directory `foo` and all its contents",
        before=[FileEntry("foo/bar")],
        after=[],
    ),
    Task(
        name="delete_dir_project_backtrack",
        prompt="delete directory `foo/../bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo")],
    ),
    Task(
        name="delete_dir_project_reentry",
        prompt="delete directory `../project/foo`",
        before=[DirEntry("foo")],
        after=[],
    ),
    Task(
        name="delete_dir_parent_backtrack",
        prompt="delete directory `../foo/../bar`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo")],
    ),
]
