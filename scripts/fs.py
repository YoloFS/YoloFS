from dataclasses import dataclass


@dataclass(frozen=True)
class FileEntry:
    path: str
    content: str | None = None
    mode: int | None = 0o644


@dataclass(frozen=True)
class DirEntry:
    path: str


@dataclass(frozen=True)
class SymlinkEntry:
    path: str
    target: str


FsEntry = FileEntry | DirEntry | SymlinkEntry
