#!/usr/bin/env -S uv run

import json
from collections import Counter
from pathlib import Path

import pandas as pd

from scripts.consts import LOG_DIR


def format_tools(labels: list[str]) -> str:
    counts = Counter(labels)
    parts = [f"{l}×{counts[l]}" if counts[l] > 1 else l for l in dict.fromkeys(labels)]
    return ", ".join(parts) or "-"


def build_report(log_dir: Path) -> pd.DataFrame:
    agents: list[str] = []
    data: dict[tuple[str, str], dict] = {}

    for agent_dir in sorted(log_dir.iterdir()):
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
            data[agent_dir.name, prompt_dir.name] = {
                "asks": result.get("asks", 0),
                "tools": format_tools(labels),
            }

    prompts = sorted({p for _, p in data})
    columns = pd.MultiIndex.from_product([agents, ["asks", "tools"]])
    rows = [
        [v for agent in agents for v in (data[agent, p].values() if (agent, p) in data else [None, "-"])]
        for p in prompts
    ]
    return pd.DataFrame(rows, index=prompts, columns=columns)


def main() -> None:
    df = build_report(LOG_DIR)
    pd.set_option("display.max_colwidth", 80)
    pd.set_option("display.width", 220)
    pd.set_option("display.max_rows", None)
    print(df.to_string())


if __name__ == "__main__":
    main()
