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
from pathlib import Path
from typing import TextIO

import pyte

ANSI_ESCAPE_RE = re.compile(
    r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\))"
)
CONTROL_RE = re.compile(r"[\x00-\x08\x0B-\x1F\x7F]")
STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()
INITIAL_INPUT_DELAY_SECONDS = 1.0


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
    command: list[str],
    input_str: str,
    raw_output_path: Path,
    screen_output_path: Path,
) -> None:
    master_fd, slave_fd = pty.openpty()
    process = subprocess.Popen(
        command,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        close_fds=True,
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

            time.sleep(3)
            os.write(master_fd, input_str.encode("utf-8"))
            time.sleep(0.5)
            os.write(master_fd, b"\n")
            time.sleep(0.5)
            os.write(master_fd, b"\r")


            while process.poll() is None:
                readable, _, _ = select.select([master_fd, STDIN_FILENO], [], [])
                if master_fd in readable:
                    handle_output(raw_output_file, screen_output_file)
                if STDIN_FILENO in readable:
                    handle_input()
    finally:
        termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
        process.kill()
        os.close(master_fd)

    print(f"Raw output saved to {raw_output_path}")
    print(f"Screen output saved to {screen_output_path}")
