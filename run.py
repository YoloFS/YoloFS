#!/usr/bin/env -S uv run

import argparse
import os
import subprocess
from pathlib import Path

from scripts.claude import ClaudeAgent
from scripts.codex import CodexAgent
from scripts.consts import PROJ_DIR, RESULTS_DIR
from scripts.prompts import ALL_PROMPTS, PROMPTS
from scripts.runner import Runner


def system(cmd: str) -> None:
    print(f"Running command: `{cmd}`")
    subprocess.run(cmd, shell=True, check=True)


AGENTS = [ClaudeAgent(), CodexAgent()]


def main(agent_name: str, data_root: Path, prompt_keys: list[str]) -> None:
    os.makedirs(RESULTS_DIR, exist_ok=True)
    agent = next(agent for agent in AGENTS if agent.name == agent_name)

    for prompt_key in prompt_keys:
        result_dir = RESULTS_DIR / f"{agent_name}" / prompt_key
        cwd = data_root / "project"
        system(f"{PROJ_DIR}/prep_fs.sh {data_root}")
        Runner(
            agent=agent, prompt_key=prompt_key, result_dir=result_dir, cwd=cwd
        ).run()


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
