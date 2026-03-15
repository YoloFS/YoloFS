#!/usr/bin/env -S uv run

import json
from dataclasses import dataclass, field
from pathlib import Path

from eval.consts import RESULTS_DIR


@dataclass
class TaskResult:
    agent: str
    task: str
    asks: int
    complete: bool
    output_success: bool
    fs_success: bool
    tool_calls: list[dict] = field(repr=False)

    @property
    def passed(self) -> bool:
        return self.output_success and self.fs_success

    @property
    def total_calls(self) -> int:
        return len(self.tool_calls)

    @property
    def commands(self) -> int:
        return sum(1 for c in self.tool_calls if not c.get("is_builtin", True))

    @property
    def failed_commands(self) -> int:
        return sum(1 for c in self.tool_calls if c.get("is_error") is True)


def load_all(results_dir: Path) -> dict[str, dict[str, TaskResult]]:
    """Load results for all agents. Returns {agent: {task_name: TaskResult}}."""
    data: dict[str, dict[str, TaskResult]] = {}
    for agent_dir in sorted(results_dir.iterdir()):
        if not agent_dir.is_dir():
            continue
        agent = agent_dir.name
        tasks: dict[str, TaskResult] = {}
        for task_dir in sorted(agent_dir.iterdir()):
            if not task_dir.is_dir():
                continue
            if task_dir.name.startswith("mini"):
                continue
            # Use the first run (run 0)
            for run_dir in sorted(task_dir.iterdir()):
                result_path = run_dir / "result.json"
                if not result_path.exists():
                    continue
                raw = json.loads(result_path.read_text())
                oc = raw.get("output_check", {})
                fc = raw.get("fs_check", {})
                tasks[task_dir.name] = TaskResult(
                    agent=agent,
                    task=task_dir.name,
                    asks=raw.get("asks", 0),
                    complete=bool(raw.get("complete", False)),
                    output_success=bool(oc.get("success", False)),
                    fs_success=bool(fc.get("success", False)),
                    tool_calls=list(raw.get("tool_calls", [])),
                )
                break  # only first run
        if tasks:
            data[agent] = tasks
    return data


def pct(n: int, d: int) -> str:
    return f"{100 * n / d:.0f}%" if d else "-"


def md_table(headers: list[str], rows: list[list[str]]) -> str:
    """Build a markdown table string."""
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(cell))

    def fmt_row(cells: list[str]) -> str:
        return "| " + " | ".join(c.ljust(widths[i]) for i, c in enumerate(cells)) + " |"

    lines = [
        fmt_row(headers),
        "| " + " | ".join("-" * widths[i] for i in range(len(headers))) + " |",
    ]
    for row in rows:
        lines.append(fmt_row(row))
    return "\n".join(lines)


import re

# Strip absolute data directory paths down to relative parts.
# Matches both .../project/foo and .../root-id/foo (parent-scope paths).
_DATA_PATH_RE = re.compile(
    r"/(?:home|users)/\S+?/data/(?:roots/)?\d{4}-\d{2}-\d{2}-\d{2}-\d{2}-\d{2}-\d+/(?:project/)?"
)


def _shorten(s: str) -> str:
    """Strip absolute data directory paths to keep output concise."""
    return _DATA_PATH_RE.sub("", s)


def format_tool_call(tc: dict) -> str:
    """Format a single tool call as a concise string."""
    name = tc.get("name", "?")
    inp = tc.get("input", {})
    err = tc.get("is_error") is True
    prefix = "[ERR] " if err else ""

    cmd = inp.get("command") or inp.get("cmd") or ""
    if cmd:
        return f"{prefix}`{_shorten(cmd)}`"
    # For builtin tools like Read, Write, Edit, Glob, Grep
    if name in ("Read", "Grep", "Glob"):
        path = inp.get("file_path") or inp.get("path") or inp.get("pattern") or ""
        path = _shorten(path)
        return f"{prefix}{name} {path}" if path else f"{prefix}{name}"
    if name in ("Write", "Edit"):
        path = _shorten(inp.get("file_path") or "")
        return f"{prefix}{name} {path}" if path else f"{prefix}{name}"
    return f"{prefix}{name}"


def generate_report(data: dict[str, dict[str, TaskResult]]) -> str:
    agents = sorted(data.keys())
    all_tasks = sorted({t for tasks in data.values() for t in tasks})
    lines: list[str] = []

    # --- Overall summary ---
    lines.append("# Report\n")
    lines.append("## Summary\n")
    headers = ["agent", "tasks", "pass", "pass%", "asks", "avg asks", "calls", "cmds", "errors"]
    rows = []
    for agent in agents:
        tasks = data[agent]
        n = len(tasks)
        passed = sum(1 for t in tasks.values() if t.passed)
        asks = sum(t.asks for t in tasks.values())
        calls = sum(t.total_calls for t in tasks.values())
        cmds = sum(t.commands for t in tasks.values())
        errors = sum(t.failed_commands for t in tasks.values())
        rows.append([
            agent, str(n), str(passed), pct(passed, n),
            str(asks), f"{asks / n:.1f}", str(calls), str(cmds), str(errors),
        ])
    lines.append(md_table(headers, rows))
    lines.append("")

    # --- Per-task table ---
    lines.append("## Tasks\n")
    headers = ["task", *agents]
    rows = []
    for task in all_tasks:
        row = [task]
        for agent in agents:
            r = data[agent].get(task)
            if r is None:
                row.append("-")
            else:
                status = "pass" if r.passed else "FAIL"
                parts = [status, f"asks={r.asks}", f"calls={r.total_calls}"]
                if r.commands:
                    parts.append(f"cmds={r.commands}")
                if r.failed_commands:
                    parts.append(f"err={r.failed_commands}")
                row.append(", ".join(parts))
        rows.append(row)
    lines.append(md_table(headers, rows))
    lines.append("")

    # --- Per-agent tool/command lists ---
    for agent in agents:
        lines.append(f"## {agent}\n")
        for task in all_tasks:
            r = data[agent].get(task)
            if r is None:
                continue
            status = "pass" if r.passed else "FAIL"
            lines.append(f"### {task} ({status})\n")
            for tc in r.tool_calls:
                lines.append(f"- {format_tool_call(tc)}")
            lines.append("")

    return "\n".join(lines)


def main() -> None:
    data = load_all(RESULTS_DIR)
    if not data:
        print(f"No results found under {RESULTS_DIR}")
        return

    report = generate_report(data)
    print(report)

    out_path = RESULTS_DIR / "report.md"
    out_path.write_text(report)
    print(f"\nSaved to {out_path}")


if __name__ == "__main__":
    main()
