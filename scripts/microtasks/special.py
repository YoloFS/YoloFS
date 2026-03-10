from scripts.microtasks.base import FileEntry, Task

SPECIAL_TASKS: list[Task] = [
    # Multi-step
    Task(
        name="copy_project_file_then_overwrite_source",
        prompt="copy file `foo` to `bar` and then overwrite `foo` with text `updated`",
        before=[FileEntry("foo", "hello")],
        after=[FileEntry("foo", "updated"), FileEntry("bar", "hello")],
    ),
]
