"""Example watcher service: every 30 seconds, emit `example.tick` with a counter kept in `state`.

The shape of every watcher: read the cursor, look for something new, emit a typed event for it,
then move the cursor (after emitting, so a crash re-emits rather than loses), and report health.
"""

import time
from collections.abc import Callable
from typing import Protocol

import botler
from shared.events import ExampleTick

INTERVAL_SECONDS = 30.0
CURSOR = "count"


class State(Protocol):
    def get(self, key: str) -> str | None: ...
    def set(self, key: str, value: str) -> None: ...


def step(state: State, emit: Callable[[ExampleTick], object], healthy: Callable[[], None]) -> int:
    count = int(state.get(CURSOR) or "0") + 1
    emit(ExampleTick(count=count))
    state.set(CURSOR, str(count))
    healthy()
    return count


def main() -> None:
    while True:
        count = step(botler.state, botler.emit, botler.health.ok)
        botler.log("ticked", count=count)
        time.sleep(INTERVAL_SECONDS)


if __name__ == "__main__":
    main()
