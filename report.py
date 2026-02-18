#!/usr/bin/env -S uv run

import json
from collections import Counter
from pathlib import Path

import pandas as pd

from scripts.consts import LOG_DIR


def get_tool_label(tc: dict) -> str:
    name = tc.get("name", "unknown")
    if tc.get("type") == "command":
        inp = tc.get("input", {})
        cmd = inp.get("cmd") or inp.get("command", "")
        return cmd.split()[0] if cmd else name
    return name


def format_tools(labels: list[str]) -> str:
    if not labels:
        return "-"
    counts = Counter(labels)
    seen = list(dict.fromkeys(labels))
    parts = [f"{label}×{counts[label]}" if counts[label] > 1 else label for label in seen]
    return ", ".join(parts)


def build_report(log_dir: Path) -> pd.DataFrame:
    agents: list[str] = []
    all_prompts: set[str] = set()
    data: dict[tuple[str, str], dict] = {}

    for agent_dir in sorted(log_dir.iterdir()):
        if not agent_dir.is_dir():
            continue
        agent = agent_dir.name
        agents.append(agent)
        for prompt_dir in sorted(agent_dir.iterdir()):
            if not prompt_dir.is_dir():
                continue
            result_path = prompt_dir / "result.json"
            if not result_path.exists():
                continue
            result = json.loads(result_path.read_text())
            labels = [get_tool_label(tc) for tc in result.get("tool_calls", [])]
            all_prompts.add(prompt_dir.name)
            data[agent, prompt_dir.name] = {
                "asks": result.get("asks", 0),
                "tools": format_tools(labels),
            }

    prompts = sorted(all_prompts)
    columns = pd.MultiIndex.from_product([agents, ["asks", "tools"]])
    rows = []
    for prompt in prompts:
        row: list = []
        for agent in agents:
            entry = data.get((agent, prompt))
            row.extend([entry["asks"], entry["tools"]] if entry else [None, "-"])
        rows.append(row)
    return pd.DataFrame(rows, index=prompts, columns=columns)


def main() -> None:
    df = build_report(LOG_DIR)
    pd.set_option("display.max_colwidth", 80)
    pd.set_option("display.width", 220)
    pd.set_option("display.max_rows", None)
    print(df.to_string())


if __name__ == "__main__":
    main()
