"""Finding agent-authored code in the monorepo and writing the generated manifests."""

from __future__ import annotations

import argparse
import importlib
import json
import pkgutil
import sys
from pathlib import Path
from typing import Any

# Workspace members whose modules may declare events and tools. Jobs are entry points, not
# libraries, so they are not imported.
ROOTS = ("packages", "tools", "connectors")


def import_all(root: Path) -> None:
    """Import every module of every `<root>/<member>/src/<package>`. Import errors propagate."""
    for kind in ROOTS:
        for src in sorted(root.glob(f"{kind}/*/src")):
            if str(src) not in sys.path:
                sys.path.insert(0, str(src))
            for pkg in sorted(p for p in src.iterdir() if (p / "__init__.py").exists()):
                importlib.import_module(pkg.name)
                for mod in pkgutil.walk_packages([str(pkg)], prefix=f"{pkg.name}."):
                    importlib.import_module(mod.name)


def manifest_parser(prog: str, filename: str) -> tuple[argparse.ArgumentParser, Any]:
    """A parser with a `manifest [--check] [--root DIR]` subcommand; returns it and its subparsers."""
    p = argparse.ArgumentParser(prog=f"python -m {prog}")
    sub = p.add_subparsers(dest="command", required=True)
    m = sub.add_parser("manifest", help=f"write {filename} from the code")
    m.add_argument("--check", action="store_true", help="fail if the committed file is stale")
    m.add_argument("--root", type=Path, default=Path("."))
    return p, sub


def write_manifest(prog: str, path: Path, entries: list[dict[str, Any]], check: bool) -> int:
    """Write `entries` to `path`, or with `check` only report whether the file is current."""
    text = json.dumps(entries, indent=2, sort_keys=True) + "\n"
    if check:
        current = path.read_text() if path.exists() else None
        if current != text:
            print(f"{path.name} is stale; run `python -m {prog} manifest` and commit it", file=sys.stderr)
            return 1
        print(f"{path.name} is current")
        return 0
    path.write_text(text)
    print(f"wrote {path}")
    return 0
