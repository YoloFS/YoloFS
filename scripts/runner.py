import difflib
import errno
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
from datetime import datetime
from enum import Enum, auto
from pathlib import Path
from typing import TextIO

import pyte

from scripts.agent import Agent
from scripts.consts import ROOTS_DIR, TERM_BLUE, TERM_GREEN, TERM_RED, TERM_RESET
from scripts.records import FsCheckResult, OutputCheckResult, ToolCall
from scripts.tasks import Task

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
        task: Task,
        result_dir: Path,
    ) -> None:
        # Run configuration.
        self.agent = agent
        self.task = task
        self.result_dir = result_dir
        self.roots_dir = ROOTS_DIR
        self.root_path = self._new_root_path()
        self.root_link = result_dir / "root"
        self.cwd = self.root_path / "project"

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
        self.last_ask_signature: str | None = None

    def run(self) -> None:
        self._prepare_run()
        print(f"{TERM_BLUE}Agent: {self.agent.name}{TERM_RESET}")
        print(f"{TERM_BLUE}Prompt: {self.task.prompt}{TERM_RESET}")
        print(f"{TERM_BLUE}Result: {self.result_dir}{TERM_RESET}")
        print(f"{TERM_BLUE}Root: {self.root_path}{TERM_RESET}")
        print(f"{TERM_BLUE}Cmd: {self.agent.command}{TERM_RESET}")
        self._start_process()
        try:
            self._run_loop()
        finally:
            self._cleanup()

        session_path = self.agent.save_session(self.cwd, self.result_dir)
        tool_calls = self.agent.extract_tool_calls(session_path) if session_path else []
        outputs_check = self.task.check_outputs(tool_calls)
        fs_check = self.task.check_fs(self.root_path, self.cwd)
        print(f"  Output check: {'pass' if outputs_check.success else 'fail'}")
        for reason in outputs_check.failed_reasons:
            print(f"    {reason}")
        print(f"  Filesystem check: {'pass' if fs_check.success else 'fail'}")
        for reason in fs_check.failed_reasons:
            print(f"    {reason}")
        success = outputs_check.success and fs_check.success
        self._write_result(tool_calls, success, outputs_check, fs_check)
        print(f"{TERM_BLUE}Result saved to {self.result_dir}{TERM_RESET}")

    def _new_root_path(self) -> Path:
        timestamp = datetime.now().strftime("%Y-%m-%d-%H-%M-%S")
        base_path = self.roots_dir / timestamp
        if not base_path.exists():
            return base_path
        index = 1
        while True:
            candidate = self.roots_dir / f"{timestamp}-{index}"
            if not candidate.exists():
                return candidate
            index += 1

    def _prepare_run(self) -> None:
        shutil.rmtree(self.result_dir, ignore_errors=True)
        self.roots_dir.mkdir(parents=True, exist_ok=True)
        self.root_path.mkdir(parents=True, exist_ok=True)
        self.result_dir.mkdir(parents=True, exist_ok=True)
        self.root_link.symlink_to(self.root_path)
        self.screens_dir.mkdir(parents=True, exist_ok=True)
        self.ask_dir.mkdir(parents=True, exist_ok=True)
        self.cwd.mkdir(parents=True, exist_ok=True)
        self.task.prep(self.root_path, self.cwd)
        self.agent.prepare_run(cwd=self.cwd, result_dir=self.result_dir)
        self.raw_output = (self.screens_dir / "raw.txt").open("w")
        self.screen_output = (self.result_dir / "screen.diff").open("w")

    def _write_result(
        self,
        tool_calls: list[ToolCall],
        success: bool,
        outputs_check: OutputCheckResult,
        fs_check: FsCheckResult,
    ) -> None:
        result = {
            "agent": self.agent.name,
            "prompt": self.task.prompt,
            "cwd": str(self.cwd),
            "asks": self.ask_index,
            "success": success,
            "checks": {
                "outputs": outputs_check.to_dict(),
                "filesystem": fs_check.to_dict(),
            },
            "tool_calls": [tc.to_dict() for tc in tool_calls],
        }
        with (self.result_dir / "result.json").open("w") as f:
            json.dump(result, f, indent=2)

    def _start_process(self) -> None:
        master_fd, slave_fd = pty.openpty()
        winsize = struct.pack("HHHH", TERMINAL_LINES, TERMINAL_COLUMNS, 0, 0)
        fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)
        env = os.environ.copy()
        env.pop("CLAUDECODE", None)
        process = subprocess.Popen(
            self.agent.command,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            close_fds=True,
            cwd=self.cwd,
            env=env,
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
            ask_signature = "\n".join(
                line.rstrip() for line in self.screen.display if line.strip()
            )
            if not self.is_in_ask or ask_signature != self.last_ask_signature:
                self.is_in_ask = True
                self.last_ask_signature = ask_signature
                self.write_ask_snapshot()
                ask_reply = self.agent.ask_reply(self.screen)
                if ask_reply is not None:
                    self._send_line(ask_reply)
            return

        self.is_in_ask = False
        self.last_ask_signature = None

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
        if self.is_in_ask or self.agent.is_ask(self.screen):
            return
        self.phase = RunPhase.WAITING_FOR_WORK_START
        self._send_line(self.task.prompt)

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
            try:
                os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        if self.master_fd is not None:
            os.close(self.master_fd)
