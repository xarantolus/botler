"""Settings the user edits for a job or service: a pydantic model the Settings page renders.

    # jobs/mail-watch/src/mail_watch/settings.py
    class Settings(botler.Settings):
        \"\"\"Which folders to watch and how often.\"\"\"
        folders: list[str] = ["INBOX"]
        every_minutes: int = Field(5, ge=1, description="How often to look")

    settings = Settings.load()

The model is found by its place (`jobs/<name>/src/<package>/settings.py`, class `Settings`), so it
belongs to job `<name>`. The gate generates `settings.manifest.json` from it; the user's values are
committed in `jobs/<name>/settings.toml`, and the hub hands them to every run as
`/botler/settings.json`. Give every field a default, so the job runs before anyone sets a value.
Never put secrets in settings: name a secret group in the job instead.
"""

from __future__ import annotations

import importlib
import inspect
import json
import os
import sys
from pathlib import Path
from typing import Any, Self

from pydantic import BaseModel, ConfigDict

from ._discover import manifest_parser, write_manifest

FILE = "/botler/settings.json"


class Settings(BaseModel):
    """Base of a runnable's settings. The class docstring is shown above the form; field
    `description`s and `title`s label the fields."""

    # A misspelled key in settings.toml is an error, not silently ignored.
    model_config = ConfigDict(extra="forbid")

    @classmethod
    def load(cls) -> Self:
        """The values the user set, with the model's defaults for the rest."""
        path = Path(os.environ.get("BOTLER_SETTINGS_FILE", FILE))
        values = json.loads(path.read_text()) if path.exists() else {}
        return cls.model_validate(values)


def manifest(root: Path) -> list[dict[str, Any]]:
    """Every job's `Settings` model in the format of `settings.manifest.json`."""
    entries: list[dict[str, Any]] = []
    for module in sorted(root.glob("jobs/*/src/*/settings.py")):
        job, package, src = module.parents[2].name, module.parent.name, module.parents[1]
        if str(src) not in sys.path:
            sys.path.insert(0, str(src))
        cls = getattr(importlib.import_module(f"{package}.settings"), "Settings", None)
        if not (isinstance(cls, type) and issubclass(cls, Settings)):
            raise TypeError(f"{module} must define `class Settings(botler.Settings)`")
        entries.append(
            {
                "job": job,
                "description": inspect.cleandoc(cls.__dict__.get("__doc__") or ""),
                "schema": cls.model_json_schema(),
            }
        )
    return entries


def main(argv: list[str] | None = None) -> int:
    parser, _ = manifest_parser("botler.settings", "settings.manifest.json")
    args = parser.parse_args(argv)
    return write_manifest("botler.settings", args.root / "settings.manifest.json", manifest(args.root), args.check)


if __name__ == "__main__":
    from botler.settings import main as _main

    sys.exit(_main())
