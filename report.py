#!/usr/bin/env -S uv run

import json
from pathlib import Path

import pandas as pd

from scripts.consts import RESULTS_DIR


def extract_commands(tool_call: dict) -> list[str]:
    commands: list[str] = []

    tool_input = tool_call.get("input")
    if isinstance(tool_input, dict):
        for key in ("cmd", "command"):
            value = tool_input.get(key)
            if isinstance(value, str) and value:
                commands.append(value)

    return commands


def build_report(results_dir: Path) -> pd.DataFrame:
    metrics = ["asks", "tools", "commands"]
    agents: list[str] = []
    data: dict[tuple[str, str], dict] = {}

    for agent_dir in sorted(results_dir.iterdir()):
        if not agent_dir.is_dir():
            continue
        agents.append(agent_dir.name)
        for prompt_dir in sorted(agent_dir.iterdir()):
            if not prompt_dir.is_dir():
                continue
            result_path = prompt_dir / "result.json"
            if not result_path.exists():
                continue
            result = json.loads(result_path.read_text())
            labels = [tc.get("name", "unknown") for tc in result.get("tool_calls", [])]
            commands = []
            for tool_call in result.get("tool_calls", []):
                commands.extend(extract_commands(tool_call))
            data[agent_dir.name, prompt_dir.name] = {
                "asks": result.get("asks", 0),
                "tools": ", ".join(labels) or "-",
                "commands": ", ".join(commands) or "-",
            }

    prompts = sorted({p for _, p in data})
    columns = pd.MultiIndex.from_product([agents, metrics])
    rows = [
        [
            data[agent, p][metric]
            if (agent, p) in data
            else (None if metric == "asks" else "-")
            for agent in agents
            for metric in metrics
        ]
        for p in prompts
    ]
    return pd.DataFrame(rows, index=prompts, columns=columns)


def main() -> None:
    df = build_report(RESULTS_DIR)
    pd.set_option("display.max_colwidth", 80)
    pd.set_option("display.width", 220)
    pd.set_option("display.max_rows", None)
    asks_df = df.xs("asks", axis=1, level=1)
    tools_df = df.xs("tools", axis=1, level=1)

    print("Asks")
    print(asks_df.to_string())
    print()
    print("Tools")
    print(tools_df.to_string())

    commands_df = df.xs("commands", axis=1, level=1)
    if (commands_df != "-").any().any():
        print()
        print("Commands")
        print(commands_df.to_string())


if __name__ == "__main__":
    main()
