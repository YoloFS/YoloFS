import difflib
import errno
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time
import tty
from enum import Enum, auto
from pathlib import Path
from typing import TextIO

import pyte

from scripts.agent import Agent

STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()
TERMINAL_COLUMNS = 120
TERMINAL_LINES = 20


class RunPhase(Enum):
    # Waiting for terminal to become ready and send the scripted prompt.
    INIT = auto()
    # Prompt has been sent; waiting for the agent to start processing.
    WAITING_FOR_WORK_START = auto()
    # A permission prompt is currently on screen and being handled.
    WAITING_FOR_PERMISSION = auto()
    # Agent is processing; waiting until it returns to input-ready state.
    WAITING_FOR_WORK_COMPLETE = auto()
    # Run completed for this prompt.
    FINISHED = auto()


class Terminal:
    def __init__(
        self,
        agent: Agent,
        prompt: str,
        log_dir: Path,
        cwd: Path,
    ) -> None:
        self.agent = agent
        self.prompt = prompt
        self.log_dir = log_dir
        self.cwd = cwd

        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.screen = pyte.Screen(TERMINAL_COLUMNS, TERMINAL_LINES)
        self.stream = pyte.Stream(self.screen, strict=False)
        self.previous_screen_lines: list[str] = []
        self.screen_diff_index = 0
        self.screens_dir = self.log_dir / "screens"
        self.is_screen_ready = False
        self.phase = RunPhase.INIT
        self.has_sent_input = False

    def run(self) -> None:
        self._start_process()
        self.screens_dir.mkdir(parents=True, exist_ok=True)
        old_stdin_attrs = None
        use_raw_stdin = os.isatty(STDIN_FILENO)
        if use_raw_stdin:
            old_stdin_attrs = termios.tcgetattr(STDIN_FILENO)
            tty.setraw(STDIN_FILENO)
        try:
            with (
                (self.log_dir / "raw.txt").open("w") as raw_output,
                (self.log_dir / "screen.diff").open("w") as screen_output,
                (self.log_dir / "permission.txt").open("w") as permission_output,
            ):
                self._run_loop(raw_output, screen_output, permission_output)
        finally:
            if old_stdin_attrs is not None:
                termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
            self._cleanup()

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

    def _run_loop(
        self,
        raw_output: TextIO,
        screen_output: TextIO,
        permission_output: TextIO,
    ) -> None:
        assert self.master_fd is not None
        assert self.process is not None
        while self.process.poll() is None:
            readable, _, _ = select.select([self.master_fd, STDIN_FILENO], [], [], 0.2)
            if self.master_fd in readable:
                should_stop = self._handle_output(
                    raw_output, screen_output, permission_output
                )
                if should_stop:
                    return
                if self._is_done():
                    return
            if STDIN_FILENO in readable:
                self._handle_input()
            self._maybe_send_prompt()

    def _handle_output(
        self,
        raw_output: TextIO,
        screen_output: TextIO,
        permission_output: TextIO,
    ) -> bool:
        assert self.master_fd is not None
        try:
            data = os.read(self.master_fd, 4096)
        except OSError as exc:
            if exc.errno == errno.EIO:
                return True
            raise

        os.write(STDOUT_FILENO, data)
        os.write(raw_output.fileno(), data)
        decoded = data.decode("utf-8", errors="replace")
        try:
            self.stream.feed(decoded)
        except TypeError:
            pass

        self.write_screen_diff(screen_output)
        self.is_screen_ready = self.agent.is_screen_ready(self.screen)
        self._advance_phase(permission_output)
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
        next_diff_index = self.screen_diff_index + 1
        diff_lines = list(
            difflib.unified_diff(
                previous_non_empty,
                current_non_empty,
                fromfile=f"screen-{self.screen_diff_index}",
                tofile=f"screen-{next_diff_index}",
                n=0,
                lineterm="",
            )
        )
        if diff_lines:
            for line in diff_lines:
                output_file.write(f"{line}\n")
            output_file.flush()
            self.screen_diff_index = next_diff_index
            self.write_screen_snapshot(self.screen_diff_index)
        self.previous_screen_lines = current_lines

    def write_screen(self, output_file: TextIO) -> None:
        for line in self.screen.display:
            output_file.write(line)
            output_file.write("\n")
        output_file.flush()

    def _advance_phase(self, permission_output: TextIO) -> None:
        is_ask = self.agent.is_ask(self.screen)
        if is_ask:
            if self.phase is not RunPhase.WAITING_FOR_PERMISSION:
                self.phase = RunPhase.WAITING_FOR_PERMISSION
                self.write_screen(permission_output)
                ask_reply = self.agent.ask_reply(self.screen)
                if ask_reply is not None:
                    self._send_line(ask_reply)
            return

        if self.phase is RunPhase.WAITING_FOR_PERMISSION:
            self.phase = (
                RunPhase.WAITING_FOR_WORK_START
                if self.has_sent_input
                else RunPhase.INIT
            )

        is_busy = self.agent.is_busy(self.screen)
        if self.phase is RunPhase.WAITING_FOR_WORK_START and is_busy:
            self.phase = RunPhase.WAITING_FOR_WORK_COMPLETE
            return

        is_waiting_for_input = self.agent.is_waiting_for_input(self.screen)
        if self.phase is RunPhase.WAITING_FOR_WORK_COMPLETE and is_waiting_for_input:
            self.phase = (
                RunPhase.FINISHED
                if self.has_sent_input
                else RunPhase.INIT
            )

    def _handle_input(self) -> None:
        assert self.master_fd is not None
        if data := os.read(STDIN_FILENO, 1024):
            os.write(self.master_fd, data)

    def _maybe_send_prompt(self) -> None:
        if self.phase is not RunPhase.INIT:
            return
        if not self.is_screen_ready or self.has_sent_input:
            return
        self.phase = RunPhase.WAITING_FOR_WORK_START
        self._send_line(self.prompt)
        self.has_sent_input = True

    def _is_done(self) -> bool:
        return self.phase is RunPhase.FINISHED

    def _send_line(self, text: str) -> None:
        assert self.master_fd is not None
        os.write(self.master_fd, text.encode("utf-8"))
        time.sleep(self.agent.input_delay)
        os.write(self.master_fd, self.agent.newline.encode("utf-8"))

    def _cleanup(self) -> None:
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
        if self.master_fd is not None:
            os.close(self.master_fd)
