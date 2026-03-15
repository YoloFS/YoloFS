#!/usr/bin/env -S uv run

import argparse
import os
import shutil

from agent import Agent
from claude import ClaudeAgent
from codex import CodexAgent
from consts import RESULTS_DIR
from copilot import CopilotAgent
from gemini import GeminiAgent
from opencode import OpenCodeAgent
from runner import Runner
from minitasks import MINITASKS
from microtasks import MICROTASKS, Task

ALL_TASKS = [*MICROTASKS, *MINITASKS]
AGENTS = [ClaudeAgent(), CodexAgent(), CopilotAgent(), GeminiAgent(), OpenCodeAgent()]
TASK_TIMEOUT = 3 * 60  # seconds


def run_task(agent: Agent, task: Task, i: int) -> None:
    result_dir = RESULTS_DIR / agent.name / task.name / str(i)
    if result_dir.exists():
        print(f"Skipping {result_dir} because it already exists")
        return
    tmp_dir = RESULTS_DIR / agent.name / task.name / f"{i}.tmp"
    if tmp_dir.exists():
        shutil.rmtree(tmp_dir)
    Runner(agent=agent, task=task, result_dir=tmp_dir, timeout=TASK_TIMEOUT).run()
    tmp_dir.rename(result_dir)


def main(agent_name: str, keys: list[str], runs: int) -> None:
    os.makedirs(RESULTS_DIR, exist_ok=True)
    agent = next(agent for agent in AGENTS if agent.name == agent_name)
    tasks = [t for t in ALL_TASKS if t.name in keys]
    for i in range(runs):
        for task in tasks:
            run_task(agent, task, i)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("agent_name", type=str)
    parser.add_argument(
        "prompts",
        type=str,
        nargs="*",
        default=[t.name for t in ALL_TASKS],
        choices=[t.name for t in ALL_TASKS],
    )
    parser.add_argument("--runs", type=int, default=1)
    args = parser.parse_args()
    prompts = args.prompts or [t.name for t in ALL_TASKS]
    main(args.agent_name, prompts, args.runs)
