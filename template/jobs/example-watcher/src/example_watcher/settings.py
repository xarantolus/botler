import botler
from pydantic import Field


class Settings(botler.Settings):
    """How often the example watcher ticks."""

    interval_seconds: float = Field(30.0, ge=5, description="Seconds between ticks")
