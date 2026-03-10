from scripts.microtasks.base import FileEntry, SymlinkEntry, Task

# Symlink Create: project, parent, symlink dir, spaces
CREATE_SYMLINK_TASKS: list[Task] = [
    Task(
        name="create_symlink_project",
        prompt="create a symlink `foo` pointing to `bar`",
        before=[FileEntry("bar", "hello")],
        after=[FileEntry("bar", "hello"), SymlinkEntry("foo", "bar")],
    ),
    Task(
        name="create_symlink_parent",
        prompt="create a symlink `../foo` pointing to `.`",
        before=[],
        after=[SymlinkEntry("../foo", ".")],
    ),
    Task(
        name="create_symlink_with_spaces",
        prompt="create a symlink `foo bar` pointing to `.`",
        before=[SymlinkEntry("foo", "."), SymlinkEntry("bar", ".")],
        after=[
            SymlinkEntry("foo", "."),
            SymlinkEntry("bar", "."),
            SymlinkEntry("foo bar", "."),
        ],
    ),
]
