#!/usr/bin/env -S uv run

import argparse
import errno
import os
import pty
import re
import select
import subprocess
import sys
import termios
import tty
from datetime import datetime
from typing import TextIO

import pyte

from scripts.consts import LOG_DIR

ANSI_ESCAPE_RE = re.compile(
    r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\))"
)
CONTROL_RE = re.compile(r"[\x00-\x08\x0B-\x1F\x7F]")
STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()


class Terminal:
    def __init__(self) -> None:
        os.makedirs(LOG_DIR, exist_ok=True)
        timestamp = datetime.now().strftime("%Y-%m-%d-%H-%M-%S")
        self.raw_output_path = LOG_DIR / f"{timestamp}-raw.txt"
        self.screen_output_path = LOG_DIR / f"{timestamp}-screen.txt"
        self.screen = pyte.Screen(*os.get_terminal_size())
        self.stream = pyte.Stream(self.screen)


def write_raw(text: str, raw_output_file: TextIO) -> None:
    text = ANSI_ESCAPE_RE.sub("", text)
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = CONTROL_RE.sub("", text)
    lines = [line for line in text.split("\n") if line.strip()]
    if not lines:
        return
    raw_output_file.write("\n".join(lines))


def write_screen(screen: pyte.Screen, screen_output_file: TextIO) -> None:
    for line in screen.display:
        line = line.rstrip()
        if not line:
            continue
        screen_output_file.write(line)
        screen_output_file.write("\n")


def relay_terminal_io(
    terminal: Terminal, master_fd: int, raw_output_file, screen_output_file
) -> None:
    while True:
        readable, _, _ = select.select([master_fd, STDIN_FILENO], [], [])

        if master_fd in readable:
            try:
                data = os.read(master_fd, 4096)
            except OSError as exc:
                if exc.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            os.write(STDOUT_FILENO, data)
            decoded = data.decode("utf-8", errors="replace")
            terminal.stream.feed(decoded)
            write_raw(decoded, raw_output_file)
            write_screen(terminal.screen, screen_output_file)

        if STDIN_FILENO in readable:
            typed = os.read(STDIN_FILENO, 1024)
            if typed:
                os.write(master_fd, typed)


def run(terminal: Terminal, command: list[str], initial_input: str | None = None) -> None:
    master_fd, slave_fd = pty.openpty()
    process = subprocess.Popen(
        command,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        close_fds=True,
    )
    os.close(slave_fd)

    old_stdin_attrs = termios.tcgetattr(STDIN_FILENO)
    tty.setraw(STDIN_FILENO)
    try:
        with (
            terminal.raw_output_path.open("w") as raw_output_file,
            terminal.screen_output_path.open("w") as screen_output_file,
        ):
            if initial_input:
                os.write(master_fd, initial_input.encode("utf-8"))
            relay_terminal_io(terminal, master_fd, raw_output_file, screen_output_file)
    finally:
        termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
        if process.poll() is None:
            process.terminate()
        process.wait()
        os.close(master_fd)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input")
    parser.add_argument("command", nargs="*", default=["claude"])
    args = parser.parse_args()

    terminal = Terminal()
    run(terminal, args.command, args.input)
    print(f"Raw output saved to {terminal.raw_output_path}")
    print(f"Screen output saved to {terminal.screen_output_path}")


if __name__ == "__main__":
    main()
