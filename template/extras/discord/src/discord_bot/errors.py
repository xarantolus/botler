"""The one error type the bridge handles from Discord."""

from __future__ import annotations


class DiscordError(RuntimeError):
    """A Discord call failed. `permanent` is True for a request Discord will never accept (bad
    request, forbidden, not found): retrying the same call is pointless."""

    def __init__(self, message: str, *, permanent: bool = False) -> None:
        super().__init__(message)
        self.permanent = permanent
