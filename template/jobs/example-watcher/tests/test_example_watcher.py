import pytest
from example_watcher.__main__ import CURSOR, step
from shared.events import ExampleTick


class FakeState:
    def __init__(self) -> None:
        self.values: dict[str, str] = {}

    def get(self, key: str) -> str | None:
        return self.values.get(key)

    def set(self, key: str, value: str) -> None:
        self.values[key] = value


def test_ticks_continue_from_the_cursor() -> None:
    state, emitted, beats = FakeState(), list[ExampleTick](), list[None]()
    for _ in range(2):
        step(state, emitted.append, lambda: beats.append(None))
    assert [e.count for e in emitted] == [1, 2]
    assert state.values[CURSOR] == "2" and len(beats) == 2

    # A restart continues where the cursor is.
    step(state, emitted.append, lambda: None)
    assert emitted[-1].model_dump(mode="json") == {"count": 3}


def test_the_cursor_moves_only_after_the_emit() -> None:
    state = FakeState()

    def failing(_: ExampleTick) -> object:
        raise RuntimeError("hub unreachable")

    with pytest.raises(RuntimeError):
        step(state, failing, lambda: None)
    assert CURSOR not in state.values, "a failed emit is retried, not skipped"
