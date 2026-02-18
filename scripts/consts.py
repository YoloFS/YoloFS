from pathlib import Path

PROJ_DIR = Path(__file__).resolve().parent.parent
RESULTS_DIR = PROJ_DIR / "data" / "results"
DATA_DIR = PROJ_DIR / "data"

TERM_RED = "\033[31m"
TERM_GREEN = "\033[32m"
TERM_YELLOW = "\033[33m"
TERM_BLUE = "\033[34m"
TERM_MAGENTA = "\033[35m"
TERM_CYAN = "\033[36m"
TERM_GRAY = "\033[37m"
TERM_RESET = "\033[0m"
