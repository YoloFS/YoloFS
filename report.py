#!/usr/bin/env -S uv run

import argparse
import json
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

from scripts.consts import RESULTS_DIR

SCOPE_SUFFIXES = [
    "project_reentry",
    "project_backtrack",
    "parent_backtrack",
    "missing_dir",
    "existing_file",
    "existing_dir",
    "with_spaces",
    "symlink_dir",
    "parent_dir",
    "project_dir",
    "nonempty",
    "symlink",
    "parent",
    "project",
]


@dataclass(frozen=True)
class Record:
    prompt: str
    run: str
    asks: int
    complete: bool
    output_success: bool
    output_failed_reasons: list[str]
    fs_success: bool
    fs_failed_reasons: list[str]
    tool_calls: list[dict]

    @property
    def overall_success(self) -> bool:
        return self.output_success and self.fs_success

    @property
    def operation(self) -> str:
        return self.prompt.split("_", 1)[0]

    @property
    def scope(self) -> str:
        for suffix in SCOPE_SUFFIXES:
            if self.prompt.endswith(f"_{suffix}"):
                return suffix
        return "other"


def extract_commands(tool_call: dict) -> list[str]:
    tool_input = tool_call.get("input")
    if not isinstance(tool_input, dict):
        return []

    commands: list[str] = []
    for key in ("cmd", "command"):
        value = tool_input.get(key)
        if isinstance(value, str) and value:
            commands.append(value)
    return commands


def load_records(results_dir: Path) -> list[Record]:
    records: list[Record] = []
    for prompt_dir in sorted(results_dir.iterdir()):
        if not prompt_dir.is_dir():
            continue
        for run_dir in sorted(prompt_dir.iterdir()):
            if not run_dir.is_dir():
                continue
            result_path = run_dir / "result.json"
            if not result_path.exists():
                continue
            data = json.loads(result_path.read_text())
            output_check = data.get("output_check", {})
            fs_check = data.get("fs_check", {})
            records.append(
                Record(
                    prompt=prompt_dir.name,
                    run=run_dir.name,
                    asks=data.get("asks", 0),
                    complete=bool(data.get("complete", False)),
                    output_success=bool(output_check.get("success", False)),
                    output_failed_reasons=list(output_check.get("failed_reasons", [])),
                    fs_success=bool(fs_check.get("success", False)),
                    fs_failed_reasons=list(fs_check.get("failed_reasons", [])),
                    tool_calls=list(data.get("tool_calls", [])),
                )
            )
    return records


def pct(numerator: int, denominator: int) -> str:
    if denominator == 0:
        return "n/a"
    return f"{100 * numerator / denominator:.1f}%"


def print_summary(records: list[Record]) -> None:
    run_count = len(records)
    prompt_count = len({record.prompt for record in records})
    output_pass = sum(1 for record in records if record.output_success)
    fs_pass = sum(1 for record in records if record.fs_success)
    overall_pass = sum(1 for record in records if record.overall_success)
    completed = sum(1 for record in records if record.complete)
    ask_total = sum(record.asks for record in records)

    print("Summary")
    print(f"  runs: {run_count}")
    print(f"  prompts: {prompt_count}")
    print(f"  complete: {completed}/{run_count} ({pct(completed, run_count)})")
    print(f"  output pass: {output_pass}/{run_count} ({pct(output_pass, run_count)})")
    print(f"  fs pass: {fs_pass}/{run_count} ({pct(fs_pass, run_count)})")
    print(f"  overall pass: {overall_pass}/{run_count} ({pct(overall_pass, run_count)})")
    print(
        f"  asks: total={ask_total}, avg={ask_total / run_count:.2f}, "
        f"max={max((record.asks for record in records), default=0)}"
    )

    ask_distribution = Counter(record.asks for record in records)
    print("  ask distribution:")
    for asks, count in sorted(ask_distribution.items()):
        print(f"    {asks}: {count}")


def print_tool_stats(records: list[Record], top_n: int) -> None:
    tool_counts: Counter[str] = Counter()
    command_counts: Counter[str] = Counter()
    total_calls = 0
    error_calls = 0

    for record in records:
        calls = record.tool_calls
        total_calls += len(calls)
        for call in calls:
            tool_counts[call.get("name", "unknown")] += 1
            if call.get("is_error") is True:
                error_calls += 1
            command_counts.update(extract_commands(call))

    print("Tools")
    print(
        f"  calls: total={total_calls}, avg={total_calls / len(records):.2f}, "
        f"errors={error_calls} ({pct(error_calls, total_calls)})"
    )
    print("  by name:")
    for name, count in tool_counts.most_common(top_n):
        print(f"    {name}: {count}")

    if command_counts:
        print(f"  top {top_n} commands:")
        for command, count in command_counts.most_common(top_n):
            print(f"    {count:>3}  {command}")


def print_failure_stats(records: list[Record], top_n: int) -> None:
    failures = [record for record in records if not record.overall_success]
    reason_counts: Counter[str] = Counter()
    for record in failures:
        reason_counts.update(record.output_failed_reasons)
        reason_counts.update(record.fs_failed_reasons)

    print("Failures")
    print(f"  failing runs: {len(failures)}")
    if not failures:
        return

    print("  failing prompts:")
    for record in sorted(failures, key=lambda r: (r.prompt, r.run)):
        status = f"output={record.output_success}, fs={record.fs_success}"
        print(f"    {record.prompt}/{record.run}: {status}")

    print(f"  top {top_n} failed reasons:")
    for reason, count in reason_counts.most_common(top_n):
        print(f"    {count:>3}  {reason}")


def print_breakdown(records: list[Record], key: str) -> None:
    totals: Counter[str] = Counter()
    passes: Counter[str] = Counter()
    asks: Counter[str] = Counter()

    for record in records:
        group = getattr(record, key)
        totals[group] += 1
        if record.overall_success:
            passes[group] += 1
        asks[group] += record.asks

    print(f"Breakdown by {key}")
    print("  group                  runs   pass   pass%   avg_asks")
    for group in sorted(totals):
        total = totals[group]
        passed = passes[group]
        avg_asks = asks[group] / total if total else 0.0
        print(
            f"  {group:22}  {total:4d}   {passed:4d}   "
            f"{pct(passed, total):>6}   {avg_asks:>8.2f}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--results-dir",
        type=Path,
        default=RESULTS_DIR / "codex",
        help="Path containing prompt/run result directories.",
    )
    parser.add_argument("--top", type=int, default=10, help="Rows for top-* output.")
    args = parser.parse_args()

    records = load_records(args.results_dir)
    if not records:
        print(f"No result.json files found under {args.results_dir}")
        return

    print(f"Analyzing {args.results_dir}")
    print()
    print_summary(records)
    print()
    print_tool_stats(records, args.top)
    print()
    print_failure_stats(records, args.top)
    print()
    print_breakdown(records, "operation")
    print()
    print_breakdown(records, "scope")


if __name__ == "__main__":
    main()
