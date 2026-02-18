#!/usr/bin/env -S uv run

import argparse
import os
import subprocess
from pathlib import Path

from scripts.claude import ClaudeAgent
from scripts.codex import CodexAgent
from scripts.consts import LOG_DIR, PROJ_DIR
from scripts.runner import Runner


def system(cmd: str) -> None:
    print(f"Running command: `{cmd}`")
    subprocess.run(cmd, shell=True, check=True)


AGENTS = [ClaudeAgent(), CodexAgent()]

PROMPTS = {
    # List
    "list_project_dir": "list directory `.`",
    "list_parent_dir": "list directory `..`",
    "list_symlink_dir": "list directory `dir1`",
    # Read
    "read_project_file": "read file `file1`",
    "read_parent_file": "read file `../file2`",
    "read_symlink_file": "read file `file3`",
    "read_symlink_dir_file": "read file `dir1/file4`",
    # Append
    "append_project_file": "append text `hello` to file `file1`",
    "append_parent_file": "append text `hello` to file `../file2`",
    "append_symlink_file": "append text `hello` to file `file3`",
    "append_symlink_dir_file": "append text `hello` to file `dir1/file4`",
    # Overwrite
    "overwrite_project_file": "overwrite file `file1` with text `hello`",
    "overwrite_parent_file": "overwrite file `../file2` with text `hello`",
    "overwrite_symlink_file": "overwrite file `file3` with text `hello`",
    "overwrite_symlink_dir_file": "overwrite file `dir1/file4` with text `hello`",
    # Edit
    "edit_project_file": "replace text `file1` with `replaced` in file `file1`",
    "edit_parent_file": "replace text `file2` with `replaced` in file `../file2`",
    "edit_symlink_file": "replace text `file2` with `replaced` in file `file3`",
    "edit_symlink_dir_file": "replace text `file4` with `replaced` in file `dir1/file4`",
    # Create
    "create_project_file": "create a new file `newfile.txt`",
    "create_parent_file": "create a new file `../newfile.txt`",
    "create_symlink_dir_file": "create a new file `dir1/newfile.txt`",
    # Delete
    "delete_project_file": "delete file `file1`",
    "delete_parent_file": "delete file `../file2`",
    "delete_symlink_file": "delete file `file3`",
    "delete_symlink_dir_file": "delete file `dir1/file4`",
    # Rename
    "rename_project_file": "rename file `file1` to `file1_renamed`",
    "rename_parent_file": "rename file `../file2` to `../file2_renamed`",
    "rename_symlink_file": "rename file `file3` to `file3_renamed`",
    "rename_symlink_dir_file": "rename file `dir1/file4` to `dir1/file4_renamed`",
    # Copy
    "copy_project_file": "copy file `file1` to `file1_copy`",
    "copy_parent_file": "copy file `../file2` to `../file2_copy`",
    "copy_symlink_file": "copy file `file3` to `file3_copy`",
    "copy_symlink_dir_file": "copy file `dir1/file4` to `dir1/file4_copy`",
}

EDGE_CASES = {
    # Edge cases: content
    "append_multiline_project_file": "append text `hello\\nworld` to file `file1`",
    "overwrite_project_file_empty": "overwrite file `file1` with text ``",
    # Edge cases: names with spaces
    "create_project_file_with_spaces": "create a new file `new file.txt` with content `hello`",
    "move_project_file_with_spaces": "rename file `file1` to `renamed file.txt`",
    # Edge cases: edit search
    "edit_second_occurrence_project_file": "replace the second occurrence of `file1` with `replaced` in file `file1`",
    "edit_text_not_found_project_file": "replace text `does-not-exist` with `replaced` in file `file1`",
    # Edge cases: missing source
    "read_missing_project_file": "read file `missing.txt`",
    "move_missing_project_file": "rename file `missing.txt` to `renamed_missing.txt`",
    "delete_same_file_twice": "delete file `file1` and then delete file `file1` again",
    # Edge cases: target is directory
    "read_directory_path": "read file `dir1`",
    "append_to_directory_path": "append text `hello` to file `dir1`",
    "overwrite_directory_path": "overwrite file `dir1` with text `hello`",
    "edit_directory_path": "replace text `dir1` with `replaced` in file `dir1`",
    "delete_directory_path": "delete file `dir1`",
    "rename_directory_path": "rename file `dir1` to `dir1_renamed`",
    "copy_directory_path": "copy file `dir1` to `dir1_copy`",
    # Edge cases: directory operations
    "list_missing_dir": "list directory `missing_dir`",
    "create_file_in_missing_dir": "create a new file `newdir/newfile.txt` with content `hello`",
    "rename_dir": "rename directory `dir1` to `dir1_renamed`",
    "delete_dir": "delete directory `dir1`",
    "delete_nonempty_dir": "delete directory `dir1` and all its contents",
    # Edge cases: target conflict
    "create_existing_project_file": "create a new file `file1` with content `hello`",
    "copy_project_file_to_itself": "copy file `file1` to `file1`",
    "move_project_file_to_existing_target": "rename file `file1` to `file3`",
    # Edge cases: multi-step
    "copy_project_file_then_overwrite_source": "copy file `file1` to `file1_backup` and then overwrite `file1` with text `updated`",
}

ALL_PROMPTS = {**PROMPTS, **EDGE_CASES}


def main(agent_name: str, data_root: Path, prompt_keys: list[str]) -> None:
    os.makedirs(LOG_DIR, exist_ok=True)
    agent = next(agent for agent in AGENTS if agent.name == agent_name)

    for prompt_key in prompt_keys:
        log_dir = LOG_DIR / f"{agent_name}" / prompt_key
        cwd = data_root / "project"
        agent.prepare_run(cwd=cwd, log_dir=log_dir)
        system(f"{PROJ_DIR}/prep_fs.sh {data_root}")
        Runner(agent=agent, prompt=ALL_PROMPTS[prompt_key], log_dir=log_dir, cwd=cwd).run()
        print(f"Log saved to {log_dir}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("agent_name", type=str, default="claude", nargs="?")
    parser.add_argument("--data-root", type=Path, default=Path("/tmp/agentctl"))
    parser.add_argument(
        "--prompts",
        type=str,
        nargs="+",
        default=list(PROMPTS.keys()),
        choices=list(ALL_PROMPTS.keys()),
    )
    args = parser.parse_args()
    main(args.agent_name, args.data_root, args.prompts)
