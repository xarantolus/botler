"""Settings for the Discord bridge, edited under Settings → Jobs & Services."""

from __future__ import annotations

import botler
from pydantic import Field


class Settings(botler.Settings):
    """The Discord DM bridge."""

    reaction_emoji: str = Field(
        "👀",
        description="Emoji the bot reacts with while a message is being handled (removed on reply).",
    )
    first_sight_minutes: int = Field(
        60,
        ge=0,
        le=1440,
        description="For a DM channel seen for the first time, how far back to look for unhandled messages.",
    )
    health_seconds: int = Field(
        15,
        ge=5,
        le=120,
        description="How often the service reports health to the hub.",
    )
