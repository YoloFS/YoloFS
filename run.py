#!/usr/bin/env -S uv run

import argparse
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

from scripts.consts import LOG_DIR, PROJ_DIR
from scripts.terminal import Agent, run


def system(cmd: str) -> None:
    print(f"Running command: `{cmd}`")
    subprocess.run(cmd, shell=True, check=True)


@dataclass(frozen=True)
class ClaudeAgent(Agent):
    name: str = "claude"
    command: tuple[str, ...] = ("claude",)
    newline: str = "\r"

    def project_dir_for_cwd(self, cwd: Path) -> Path:
        project_name = str(cwd.resolve()).replace("/", "-")
        return Path.home() / ".claude" / "projects" / project_name

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        project_dir.mkdir(parents=True, exist_ok=True)
        for entry in project_dir.iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        jsonl_files = [
            path
            for path in project_dir.iterdir()
            if path.is_file() and path.suffix == ".jsonl"
        ]
        if not jsonl_files:
            print(f"No .jsonl file found in {project_dir}")
            return
        latest_jsonl = max(jsonl_files, key=lambda path: path.stat().st_mtime)
        latest_jsonl.rename(log_dir / "conversation.jsonl")


AGENTS = [ClaudeAgent()]

PROMPTS = {
    "list_curr_dir": "list directory `.`",
    "list_parent_dir": "list directory `..`",
    "list_symlink_dir": "list directory `dir1`",
}


def main(agent_name: str, data_root: Path, prompt_keys: list[str]) -> None:
    os.makedirs(LOG_DIR, exist_ok=True)
    agent = next(agent for agent in AGENTS if agent.name == agent_name)

    for prompt_key in prompt_keys:
        log_dir = LOG_DIR / f"{agent_name}" / prompt_key
        log_dir.mkdir(parents=True, exist_ok=True)
        cwd = data_root / "project"
        agent.prepare_run(cwd=cwd, log_dir=log_dir)
        system(f"{PROJ_DIR}/prep_fs.sh {data_root}")
        run(
            agent=agent,
            input_lines=[PROMPTS[prompt_key]],
            log_dir=log_dir,
            cwd=cwd,
        )
        agent.finalize_run(cwd=cwd, log_dir=log_dir)
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
        choices=list(PROMPTS.keys()),
    )
    args = parser.parse_args()
    main(args.agent_name, args.data_root, args.prompts)
