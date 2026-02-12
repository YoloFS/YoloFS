#!/usr/bin/env -S uv run

import argparse
import os
from datetime import datetime

from scripts.consts import LOG_DIR
from scripts.terminal import Agent, run

agents = [
    Agent(name="claude", command=["claude"], newline="\r"),
]

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("agent_name", type=str, default="claude", nargs="?")
    args = parser.parse_args()

    os.makedirs(LOG_DIR, exist_ok=True)
    timestamp = datetime.now().strftime("%Y-%m-%d-%H-%M-%S")
    agent = next(agent for agent in agents if agent.name == args.agent_name)
    run(
        agent=agent,
        input_lines=["hi", "/exit"],
        raw_output_path=LOG_DIR / f"{timestamp}-{agent.name}-raw.txt",
        screen_output_path=LOG_DIR / f"{timestamp}-{agent.name}-screen.txt",
    )


if __name__ == "__main__":
    main()
