import botler
from browser_example.__main__ import Bridge, State
from shared.events import ExampleLoggedIn, ExampleLoginNeeded, ExamplePageItem


class FakeState:
    def __init__(self) -> None:
        self.values: dict[str, str] = {}

    def get(self, key: str) -> str | None:
        return self.values.get(key)

    def set(self, key: str, value: str) -> None:
        self.values[key] = value


def bridge(state: State | None = None) -> tuple[Bridge, list[botler.Event], list[str]]:
    emitted: list[botler.Event] = []
    passwords: list[str] = []

    def new_password() -> str:
        passwords.append(f"pw{len(passwords)}")
        return passwords[-1]

    return Bridge(state or FakeState(), emitted.append, new_password), emitted, passwords


def item(i: str) -> dict[str, str]:
    return {"id": i, "text": f"item {i}", "url": f"https://example.com/{i}"}


def test_polled_items_are_validated_and_emitted_once() -> None:
    b, emitted, _ = bridge()
    b.items([item("a"), {"id": "b"}, item("a"), "junk"])
    b.items([item("a"), item("c")])
    b.items({"not": "a list"})
    assert [e.id for e in emitted if isinstance(e, ExamplePageItem)] == ["a", "c"]

    # The seen ids survive a restart.
    again, emitted, _ = bridge(b.state)
    again.items([item("c"), item("d")])
    assert [e.id for e in emitted if isinstance(e, ExamplePageItem)] == ["d"]


def test_pushed_signals_become_items() -> None:
    b, emitted, _ = bridge()
    signal = {"method": "Runtime.bindingCalled", "params": {"name": "botlerSignal", "payload": '{"id": "x",'}}
    good = {
        "method": "Runtime.bindingCalled",
        "params": {"name": "botlerSignal", "payload": '{"id": "y", "text": "t", "url": "u"}'},
    }
    other: dict[str, object] = {"method": "Page.frameNavigated", "params": {}}
    b.pushed([signal, other, good])
    assert [e.model_dump() for e in emitted] == [{"id": "y", "text": "t", "url": "u"}]


def test_login_changes_are_announced_with_a_fresh_password() -> None:
    b, emitted, passwords = bridge()
    b.check_login(False)
    b.check_login(False)
    b.check_login(True)
    b.check_login(True)
    assert len(emitted) == 2 and passwords == ["pw0", "pw1"], "rotated on each change, not on each check"
    assert isinstance(emitted[0], ExampleLoginNeeded) and emitted[0].vnc_password == passwords[0]
    assert isinstance(emitted[1], ExampleLoggedIn)

    # Logged in before a restart: nothing to say. Logged out: announced again with a new password.
    again, emitted, _ = bridge(b.state)
    again.check_login(True)
    assert emitted == []
    b.check_login(False)
    out, emitted, _ = bridge(b.state)
    out.check_login(False)
    assert isinstance(emitted[0], ExampleLoginNeeded)
