#!/usr/bin/env -S uv run

import argparse
import json
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pyte
from scripts.consts import LOG_DIR, PROJ_DIR
from scripts.terminal import Agent, run


def system(cmd: str) -> None:
    print(f"Running command: `{cmd}`")
    subprocess.run(cmd, shell=True, check=True)


@dataclass(frozen=True)
class ClaudeAgent(Agent):
    name: str = "claude"
    command: tuple[str, ...] = ("claude",)
    newline: str = "\r"

    def is_ask(self, screen: pyte.Screen) -> bool:
        return any("1. Yes" in line for line in screen.display)

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return "1"

    def project_dir_for_cwd(self, cwd: Path) -> Path:
        project_name = str(cwd.resolve()).replace("/", "-")
        return Path.home() / ".claude" / "projects" / project_name

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        project_dir.mkdir(parents=True, exist_ok=True)
        for entry in project_dir.iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        project_dir = self.project_dir_for_cwd(cwd)
        jsonl_files = [
            path
            for path in project_dir.iterdir()
            if path.is_file() and path.suffix == ".jsonl"
        ]
        if not jsonl_files:
            print(f"No .jsonl file found in {project_dir}")
            return
        latest_jsonl = max(jsonl_files, key=lambda path: path.stat().st_mtime)
        conversation_path = log_dir / "conversation.jsonl"
        latest_jsonl.rename(conversation_path)
        self.extract_command_results(conversation_path, log_dir / "command.jsonl")

    def extract_command_results(
        self, conversation_path: Path, output_path: Path
    ) -> None:
        results: list[dict[str, object]] = []

        with conversation_path.open("r", encoding="utf-8") as file:
            for line in file:
                line = line.strip()
                if not line:
                    continue
                event = json.loads(line)

                message = event.get("message")
                if not isinstance(message, dict):
                    continue

                content = message.get("content")
                if not isinstance(content, list):
                    continue

                for item in content:
                    if item.get("type") not in {"tool_use", "tool_result"}:
                        continue
                    results.append(item)

        with output_path.open("w", encoding="utf-8") as file:
            for record in results:
                file.write(json.dumps(record))
                file.write("\n")


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
        run(
            agent=agent,
            input_lines=[PROMPTS[prompt_key]],
            log_dir=log_dir,
            cwd=cwd,
        )
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
