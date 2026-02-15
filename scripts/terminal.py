import errno
import os
import pty
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

STDIN_FILENO = sys.stdin.fileno()
STDOUT_FILENO = sys.stdout.fileno()


@dataclass(frozen=True)
class Agent:
    name: str
    command: tuple[str, ...]
    newline: str = "\n"
    input_delay: float = 1.0

    def prepare_run(self, cwd: Path, log_dir: Path) -> None:
        pass

    def finalize_run(self, cwd: Path, log_dir: Path) -> None:
        pass

    def is_screen_ready(self, screen: pyte.Screen) -> bool:
        return any(line.strip() for line in screen.display)

    def is_ask(self, screen: pyte.Screen) -> bool:
        return False

    def ask_reply(self, screen: pyte.Screen) -> str | None:
        return None


def write_screen(screen: pyte.Screen, screen_output_file: TextIO) -> None:
    for line in screen.display:
        screen_output_file.write(line)
        screen_output_file.write("\n")
    screen_output_file.flush()


def run(
    agent: Agent,
    input_lines: list[str],
    log_dir: Path,
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

    screen = pyte.Screen(80, 24)
    stream = pyte.Stream(screen, strict=False)

    old_stdin_attrs = termios.tcgetattr(STDIN_FILENO)
    tty.setraw(STDIN_FILENO)
    try:
        with (
            (log_dir / "raw.txt").open("w", encoding="utf-8") as raw_output_file,
            (log_dir / "screen.txt").open("w", encoding="utf-8") as screen_output_file,
            (log_dir / "permission.txt").open(
                "w", encoding="utf-8"
            ) as permission_output_file,
        ):
            is_screen_ready = False
            was_ask = False
            while process.poll() is None:
                readable, _, _ = select.select([master_fd, STDIN_FILENO], [], [])
                if master_fd in readable:
                    try:
                        data = os.read(master_fd, 4096)
                    except OSError as exc:
                        if exc.errno == errno.EIO:
                            return
                        raise
                    os.write(STDOUT_FILENO, data)
                    os.write(raw_output_file.fileno(), data)
                    decoded = data.decode("utf-8", errors="replace")
                    try:
                        stream.feed(decoded)
                    except TypeError:
                        pass
                    write_screen(screen, screen_output_file)
                    is_ask = agent.is_ask(screen)
                    if is_ask and not was_ask:
                        write_screen(screen, permission_output_file)
                        ask_reply = agent.ask_reply(screen)
                        if ask_reply is not None:
                            os.write(master_fd, ask_reply.encode("utf-8"))
                            time.sleep(agent.input_delay)
                    was_ask = is_ask
                    is_screen_ready = agent.is_screen_ready(screen)
                if STDIN_FILENO in readable:
                    if data := os.read(STDIN_FILENO, 1024):
                        os.write(master_fd, data)
                if is_screen_ready and input_lines:
                    os.write(master_fd, input_lines[0].encode("utf-8"))
                    time.sleep(agent.input_delay)
                    os.write(master_fd, agent.newline.encode("utf-8"))
                    input_lines.pop(0)
    finally:
        termios.tcsetattr(STDIN_FILENO, termios.TCSADRAIN, old_stdin_attrs)
        process.kill()
        os.close(master_fd)
