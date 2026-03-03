#!/usr/bin/env -S uv run

import argparse
import os

from scripts.claude import ClaudeAgent
from scripts.codex import CodexAgent
from scripts.gemini import GeminiAgent
from scripts.opencode import OpenCodeAgent
from scripts.consts import RESULTS_DIR
from scripts.tasks import ALL_TASKS, TASKS
from scripts.runner import Runner


AGENTS = [ClaudeAgent(), CodexAgent(), GeminiAgent(), OpenCodeAgent()]


def main(agent_name: str, prompt_keys: list[str]) -> None:
    os.makedirs(RESULTS_DIR, exist_ok=True)
    agent = next(agent for agent in AGENTS if agent.name == agent_name)
    tasks = [t for t in ALL_TASKS if t.name in prompt_keys]

    for task in tasks:
        result_dir = RESULTS_DIR / f"{agent_name}" / task.name
        Runner(agent=agent, task=task, result_dir=result_dir).run()



if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("agent_name", type=str, default="claude", nargs="?")
    parser.add_argument(
        "--prompts",
        type=str,
        nargs="+",
        default=[t.name for t in TASKS],
        choices=[t.name for t in ALL_TASKS],
    )
    args = parser.parse_args()
    main(args.agent_name, args.prompts)
