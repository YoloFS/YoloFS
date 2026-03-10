from scripts.microtasks.base import DirEntry, FileEntry, Task

# Dir Create: project, parent, spaces, missing parent
CREATE_DIR_TASKS: list[Task] = [
    Task(
        name="create_dir_project",
        prompt="create a new directory `foo`",
        after=[DirEntry("foo")],
    ),
    Task(
        name="create_dir_parent",
        prompt="create a new directory `../foo`",
        after=[DirEntry("../foo")],
    ),
    Task(
        name="create_dir_with_spaces",
        prompt="create a new directory `foo bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="create_file_in_missing_dir",
        prompt="create a new file `bar/foo` with content `hello`",
        after=[DirEntry("bar"), FileEntry("bar/foo", "hello")],
    ),
    Task(
        name="create_dir_in_missing_dir",
        prompt="create a new directory `bar/foo`",
        after=[DirEntry("bar"), DirEntry("bar/foo")],
    ),
    Task(
        name="create_dir_project_backtrack",
        prompt="create a new directory `foo/../bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="create_dir_project_reentry",
        prompt="create a new directory `../project/foo`",
        after=[DirEntry("foo")],
    ),
    Task(
        name="create_dir_parent_backtrack",
        prompt="create a new directory `../foo/../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), DirEntry("../bar")],
    ),
]
