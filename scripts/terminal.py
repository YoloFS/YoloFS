import errno
import os
import pty
import re
import select
import subprocess
import sys
import termios
import time
import tty
from dataclasses import dataclass
from pathlib import Path
from typing import TextIO

import pyte

ANSI_ESCAPE_RE = re.compile(
    r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\))"
)
CONTROL_RE = re.compile(r"[\x00-\x08\x0B-\x1F\x7F]")
STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()


@dataclass(frozen=True)
class Agent:
    name: str
    command: list[str]
    newline: str = "\n"
    start_delay: float = 3.0
    input_delay: float = 0.5


def write_raw(text: str, raw_output_file: TextIO) -> None:
    text = ANSI_ESCAPE_RE.sub("", text)
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = CONTROL_RE.sub("", text)
    lines = [line for line in text.split("\n") if line.strip()]
    if not lines:
        return
    raw_output_file.write("\n".join(lines))
    raw_output_file.flush()


def write_screen(screen: pyte.Screen, screen_output_file: TextIO) -> None:
    for line in screen.display:
        line = line.rstrip()
        if not line:
            continue
        screen_output_file.write(line)
        screen_output_file.write("\n")
    screen_output_file.flush()


def run(
    agent: Agent,
    input_lines: list[str],
    raw_output_path: Path,
    screen_output_path: Path,
    cwd: Path,
) -> None:
    master_fd, slave_fd = pty.openpty()
    process = subprocess.Popen(
        agent.command,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        close_fds=True,
        cwd=cwd,
    )
    os.close(slave_fd)

    screen = pyte.Screen(*os.get_terminal_size())
    stream = pyte.Stream(screen, strict=False)

    def handle_output(raw_output_file: TextIO, screen_output_file: TextIO):
        try:
            data = os.read(master_fd, 4096)
        except OSError as exc:
            if exc.errno == errno.EIO:
                return
            raise
        if not data:
            return
        os.write(STDOUT_FILENO, data)
        decoded = data.decode("utf-8", errors="replace")
        try:
            stream.feed(decoded)
        except TypeError:
            pass
        write_raw(decoded, raw_output_file)
        write_screen(screen, screen_output_file)

    def handle_input():
        data = os.read(STDIN_FILENO, 1024)
        if data:
            os.write(master_fd, data)

    old_stdin_attrs = termios.tcgetattr(STDIN_FILENO)
    tty.setraw(STDIN_FILENO)
    try:
        with (
            raw_output_path.open("w", encoding="utf-8") as raw_output_file,
            screen_output_path.open("w", encoding="utf-8") as screen_output_file,
        ):
            handle_output(raw_output_file, screen_output_file)

            if input_lines:
                time.sleep(agent.start_delay)

            while process.poll() is None:
                readable, _, _ = select.select([master_fd, STDIN_FILENO], [], [])
                if master_fd in readable:
                    handle_output(raw_output_file, screen_output_file)
                if STDIN_FILENO in readable:
                    handle_input()
                if input_lines:
                    os.write(master_fd, input_lines[0].encode("utf-8"))
                    time.sleep(agent.input_delay)
                    os.write(master_fd, agent.newline.encode("utf-8"))
                    input_lines.pop(0)
    finally:
        termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
        process.kill()
        os.close(master_fd)

    print(f"Raw output saved to {raw_output_path}")
    print(f"Screen output saved to {screen_output_path}")
