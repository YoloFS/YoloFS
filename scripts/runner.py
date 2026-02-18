import difflib
import errno
import fcntl
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import termios
import time
from enum import Enum, auto
from pathlib import Path
from typing import TextIO

import pyte

from scripts.agent import Agent, ToolCall
from scripts.consts import TERM_BLUE, TERM_GREEN, TERM_RED, TERM_RESET
from scripts.prompts import ALL_PROMPTS

STDOUT_FILENO = sys.stdout.fileno()
TERMINAL_COLUMNS = 100
TERMINAL_LINES = 500


class RunPhase(Enum):
    # Waiting for terminal to become ready and send the scripted prompt.
    INIT = auto()
    # Prompt has been sent; waiting for the agent to start processing.
    WAITING_FOR_WORK_START = auto()
    # Agent is processing; waiting until it returns to input-ready state.
    WAITING_FOR_WORK_COMPLETE = auto()
    # Brief grace period after completion before shutdown.
    DRAINING_OUTPUT = auto()
    # Run completed for this prompt.
    FINISHED = auto()


class Runner:
    def __init__(
        self,
        agent: Agent,
        prompt_key: str,
        result_dir: Path,
        cwd: Path,
    ) -> None:
        # Run configuration.
        self.agent = agent
        self.prompt_key = prompt_key
        self.prompt = ALL_PROMPTS[prompt_key]
        self.result_dir = result_dir
        self.cwd = cwd

        # Log paths and directories.
        self.screens_dir = self.result_dir / "screens"
        self.ask_dir = self.result_dir / "ask"
        self.raw_output: TextIO | None = None
        self.screen_output: TextIO | None = None

        # Child process/PTY handles.
        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None

        # Virtual screen state.
        self.screen = pyte.Screen(TERMINAL_COLUMNS, TERMINAL_LINES)
        self.stream = pyte.Stream(self.screen, strict=False)
        self.previous_screen_lines: list[str] = []
        self.screen_index = 0
        self.ask_index = 0

        # Run state machine.
        self.phase = RunPhase.INIT
        self.is_in_ask = False

    def run(self) -> None:
        self._prepare_run()
        print(f"{TERM_BLUE}Agent: {self.agent.name}{TERM_RESET}")
        print(f"{TERM_BLUE}Prompt: {self.prompt}{TERM_RESET}")
        print(f"{TERM_BLUE}Result: {self.result_dir}{TERM_RESET}")
        self._start_process()
        try:
            self._run_loop()
        finally:
            self._cleanup()

        session_path = self.agent.save_session(self.cwd, self.result_dir)
        tool_calls = self.agent.extract_tool_calls(session_path) if session_path else []
        self._write_result(tool_calls)
        print(f"{TERM_BLUE}Result saved to {self.result_dir}{TERM_RESET}")

    def _prepare_run(self) -> None:
        shutil.rmtree(self.result_dir, ignore_errors=True)
        self.agent.prepare_run(cwd=self.cwd, result_dir=self.result_dir)
        self.result_dir.mkdir(parents=True, exist_ok=True)
        self.screens_dir.mkdir(parents=True, exist_ok=True)
        self.ask_dir.mkdir(parents=True, exist_ok=True)
        self.raw_output = (self.screens_dir / "raw.txt").open("w")
        self.screen_output = (self.result_dir / "screen.diff").open("w")

    def _write_result(self, tool_calls: list[ToolCall]) -> None:
        result = {
            "agent": self.agent.name,
            "prompt": self.prompt,
            "cwd": str(self.cwd),
            "asks": self.ask_index,
            "tool_calls": [tc.to_dict() for tc in tool_calls],
        }
        with (self.result_dir / "result.json").open("w") as f:
            json.dump(result, f, indent=2)

    def _start_process(self) -> None:
        master_fd, slave_fd = pty.openpty()
        winsize = struct.pack("HHHH", TERMINAL_LINES, TERMINAL_COLUMNS, 0, 0)
        fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)
        process = subprocess.Popen(
            self.agent.command,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            close_fds=True,
            cwd=self.cwd,
            preexec_fn=lambda: os.setsid(),  # Detach from terminal.
        )
        os.close(slave_fd)
        self.master_fd = master_fd
        self.process = process

    def _run_loop(self) -> None:
        assert self.master_fd is not None
        assert self.process is not None
        while self.process.poll() is None:
            readable, _, _ = select.select(
                [self.master_fd], [], [], self.agent.select_timeout
            )
            if self.master_fd in readable:
                should_stop = self._handle_output()
                if should_stop:
                    return
                if self._is_done():
                    self.phase = RunPhase.DRAINING_OUTPUT
                if self.phase is RunPhase.DRAINING_OUTPUT:
                    continue
            elif self.phase is RunPhase.DRAINING_OUTPUT:
                return

            self._maybe_send_prompt()

    def _handle_output(self) -> bool:
        assert self.master_fd is not None
        assert self.raw_output is not None
        assert self.screen_output is not None
        try:
            data = os.read(self.master_fd, 4096)
        except OSError as exc:
            if exc.errno == errno.EIO:
                return True
            raise

        os.write(self.raw_output.fileno(), data)
        decoded = data.decode("utf-8", errors="replace")
        try:
            self.stream.feed(decoded)
        except TypeError:
            pass

        self.write_screen_diff(self.screen_output)
        self._advance_phase()
        return False

    def write_screen_snapshot(self, index: int) -> None:
        screen_path = self.screens_dir / f"{index}.txt"
        with screen_path.open("w") as output_file:
            self.write_screen(output_file)

    def write_screen_diff(self, output_file: TextIO) -> None:
        current_lines = list(self.screen.display)
        if not self.previous_screen_lines and all(line == "" for line in current_lines):
            self.previous_screen_lines = current_lines
            return
        previous_non_empty = [
            line for line in self.previous_screen_lines if line.strip()
        ]
        current_non_empty = [line for line in current_lines if line.strip()]
        next_index = self.screen_index + 1
        diff_lines = list(
            difflib.unified_diff(
                previous_non_empty,
                current_non_empty,
                fromfile=f"screen-{self.screen_index}",
                tofile=f"screen-{next_index}",
                n=0,
                lineterm="",
            )
        )
        if diff_lines:
            for line in diff_lines:
                output_file.write(f"{line}\n")
            output_file.flush()
            for line in diff_lines[2:]:
                if line.startswith("+"):
                    print(f"{TERM_GREEN}{line.rstrip()}{TERM_RESET}")
                elif line.startswith("-"):
                    print(f"{TERM_RED}{line.rstrip()}{TERM_RESET}")
            self.screen_index = next_index
            self.write_screen_snapshot(self.screen_index)
        self.previous_screen_lines = current_lines

    def write_screen(self, output_file: TextIO) -> None:
        lines = list(self.screen.display)
        while lines and not lines[-1].strip():
            lines.pop()
        for line in lines:
            output_file.write(line)
            output_file.write("\n")
        output_file.flush()

    def write_ask_snapshot(self) -> None:
        ask_path = self.ask_dir / f"{self.ask_index}.txt"
        with ask_path.open("w") as output_file:
            self.write_screen(output_file)
        self.ask_index += 1

    def _advance_phase(self) -> None:
        is_ask = self.agent.is_ask(self.screen)
        if is_ask:
            if not self.is_in_ask:
                self.is_in_ask = True
                self.write_ask_snapshot()
                ask_reply = self.agent.ask_reply(self.screen)
                if ask_reply is not None:
                    self._send_line(ask_reply)
            return

        self.is_in_ask = False

        is_busy = self.agent.is_busy(self.screen)
        if self.phase is RunPhase.WAITING_FOR_WORK_START and is_busy:
            self.phase = RunPhase.WAITING_FOR_WORK_COMPLETE
            return

        is_waiting_for_input = self.agent.is_waiting_for_input(self.screen)
        if self.phase is RunPhase.WAITING_FOR_WORK_COMPLETE and is_waiting_for_input:
            self.phase = RunPhase.FINISHED

    def _maybe_send_prompt(self) -> None:
        if self.phase is not RunPhase.INIT:
            return
        if not self.agent.is_screen_ready(self.screen):
            return
        self.phase = RunPhase.WAITING_FOR_WORK_START
        self._send_line(self.prompt)

    def _is_done(self) -> bool:
        return self.phase is RunPhase.FINISHED

    def _send_line(self, text: str) -> None:
        assert self.master_fd is not None
        os.write(self.master_fd, text.encode("utf-8"))
        time.sleep(self.agent.input_delay)
        os.write(self.master_fd, self.agent.newline.encode("utf-8"))

    def _cleanup(self) -> None:
        if self.raw_output is not None:
            self.raw_output.close()
        if self.screen_output is not None:
            self.screen_output.close()
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
        if self.master_fd is not None:
            os.close(self.master_fd)
