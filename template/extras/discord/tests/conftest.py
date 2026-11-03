import sys
from pathlib import Path

# `--import-mode=importlib` does not put the tests directory on sys.path; the tests share `fakes`.
sys.path.insert(0, str(Path(__file__).parent))
