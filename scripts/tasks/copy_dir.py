from scripts.tasks.base import DirEntry, FileEntry, Task

# Dir Copy: project, parent, spaces, non-empty
COPY_DIR_TASKS: list[Task] = [
    Task(
        name="copy_dir_project",
        prompt="copy directory `foo` to `bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="copy_dir_parent",
        prompt="copy directory `../foo` to `../bar`",
        before=[DirEntry("../foo")],
        after=[DirEntry("../foo"), DirEntry("../bar")],
    ),
    Task(
        name="copy_dir_with_spaces",
        prompt="copy directory `foo bar` to `bar baz`",
        before=[DirEntry("foo bar"), DirEntry("foo"), DirEntry("bar")],
        after=[
            DirEntry("foo bar"),
            DirEntry("bar baz"),
            DirEntry("foo"),
            DirEntry("bar"),
        ],
    ),
    Task(
        name="copy_dir_nonempty",
        prompt="copy directory `foo` to `bar` including all files and subdirectories",
        before=[FileEntry("foo/a.txt", "A"), FileEntry("foo/sub/b.txt", "B")],
        after=[
            DirEntry("foo"),
            DirEntry("foo/sub"),
            FileEntry("foo/a.txt", "A"),
            FileEntry("foo/sub/b.txt", "B"),
            DirEntry("bar"),
            DirEntry("bar/sub"),
            FileEntry("bar/a.txt", "A"),
            FileEntry("bar/sub/b.txt", "B"),
        ],
    ),
    Task(
        name="copy_dir_project_backtrack",
        prompt="copy directory `foo/../bar` to `baz`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("bar"), DirEntry("baz")],
    ),
    Task(
        name="copy_dir_project_reentry",
        prompt="copy directory `../project/foo` to `../project/bar`",
        before=[DirEntry("foo")],
        after=[DirEntry("foo"), DirEntry("bar")],
    ),
    Task(
        name="copy_dir_parent_backtrack",
        prompt="copy directory `../bar` to `../foo/../baz`",
        before=[DirEntry("../foo"), DirEntry("../bar")],
        after=[DirEntry("../foo"), DirEntry("../bar"), DirEntry("../baz")],
    ),
    Task(
        name="copy_dir_to_existing_dir",
        prompt="copy directory `foo` to `bar`",
        before=[DirEntry("foo"), DirEntry("bar")],
        after=[DirEntry("foo"), DirEntry("bar"), DirEntry("bar/foo")],
    ),
]
