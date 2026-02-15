#!/usr/bin/env -S uv run

import argparse
import os
import subprocess
from pathlib import Path

from scripts.agent import ClaudeAgent
from scripts.consts import LOG_DIR, PROJ_DIR
from scripts.terminal import Terminal


def system(cmd: str) -> None:
    print(f"Running command: `{cmd}`")
    subprocess.run(cmd, shell=True, check=True)


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
        Terminal(
            agent=agent,
            prompt=PROMPTS[prompt_key],
            log_dir=log_dir,
            cwd=cwd,
        ).run()
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
