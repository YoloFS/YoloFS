#!/usr/bin/env -S uv run

import argparse
import os
from datetime import datetime

from scripts.consts import LOG_DIR
from scripts.terminal import run


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", nargs="*", default=["claude"])
    args = parser.parse_args()

    os.makedirs(LOG_DIR, exist_ok=True)
    timestamp = datetime.now().strftime("%Y-%m-%d-%H-%M-%S")
    run(
        command=args.command,
        input_str="/exit",
        raw_output_path=LOG_DIR / f"{timestamp}-raw.txt",
        screen_output_path=LOG_DIR / f"{timestamp}-screen.txt",
    )


if __name__ == "__main__":
    main()
