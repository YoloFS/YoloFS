import errno
import os
import pty
import select
import subprocess
import sys
import termios
import time
import tty
from pathlib import Path
from typing import TextIO

import pyte

from scripts.agent import Agent

STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()


def write_screen(screen: pyte.Screen, output_file: TextIO) -> None:
    for line in screen.display:
        output_file.write(line)
        output_file.write("\n")
    output_file.flush()


class Terminal:
    def __init__(
        self,
        agent: Agent,
        input_lines: list[str],
        log_dir: Path,
        cwd: Path,
    ) -> None:
        self.agent = agent
        self.input_lines = input_lines
        self.log_dir = log_dir
        self.cwd = cwd

        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.screen = pyte.Screen(80, 24)
        self.stream = pyte.Stream(self.screen, strict=False)
        self.is_screen_ready = False
        self.was_ask = False

    def run(self) -> None:
        self._start_process()
        old_stdin_attrs = termios.tcgetattr(STDIN_FILENO)
        tty.setraw(STDIN_FILENO)
        try:
            with (
                (self.log_dir / "raw.txt").open("w") as raw_output,
                (self.log_dir / "screen.txt").open("w") as screen_output,
                (self.log_dir / "permission.txt").open("w") as permission_output,
            ):
                self._run_loop(raw_output, screen_output, permission_output)
        finally:
            termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
            self._cleanup()

    def _start_process(self) -> None:
        master_fd, slave_fd = pty.openpty()
        process = subprocess.Popen(
            self.agent.command,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            close_fds=True,
            cwd=self.cwd,
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
            readable, _, _ = select.select([self.master_fd, STDIN_FILENO], [], [])
            if self.master_fd in readable:
                should_stop = self._handle_output(
                    raw_output, screen_output, permission_output
                )
                if should_stop:
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

        write_screen(self.screen, screen_output)
        self._maybe_handle_permission(permission_output)
        self.is_screen_ready = self.agent.is_screen_ready(self.screen)
        return False

    def _maybe_handle_permission(self, permission_output: TextIO) -> None:
        is_ask = self.agent.is_ask(self.screen)
        if is_ask and not self.was_ask:
            write_screen(self.screen, permission_output)
            ask_reply = self.agent.ask_reply(self.screen)
            if ask_reply is not None:
                self._send_line(ask_reply)
        self.was_ask = is_ask

    def _handle_input(self) -> None:
        assert self.master_fd is not None
        if data := os.read(STDIN_FILENO, 1024):
            os.write(self.master_fd, data)

    def _maybe_send_prompt(self) -> None:
        if not self.is_screen_ready or not self.input_lines:
            return
        input_line = self.input_lines.pop(0)
        self._send_line(input_line)

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
