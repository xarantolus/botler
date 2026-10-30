"""The bridge: from a logged-in Chromium (over CDP on localhost) to typed botler events.

Every POLL_SECONDS it collects what the page pushed through `botlerSignal`, evaluates POLL_JS,
checks the login, and reports health. Page results are untrusted input: they are validated into
pydantic events, and anything malformed is logged and dropped. A broken browser or CDP connection
raises, so the service exits and the hub restarts it and reports the failure.

A logged-out session is not a failure: restarting would not log it in, and the service has to
keep running for the user's login visit. Instead it emits `example.login_needed` with a fresh VNC
password, and `example.logged_in` once the login check passes again (rotating the password, so
the visit link stops working).
"""

import json
import os
import secrets
import string
from collections.abc import Callable
from pathlib import Path
from typing import Any, Protocol

import botler
from pydantic import ValidationError
from shared.events import ExampleLoggedIn, ExampleLoginNeeded, ExamplePageItem

from browser_example import config
from browser_example.cdp import Cdp, page_ws_url

SEEN_LIMIT = 500


class State(Protocol):
    def get(self, key: str) -> str | None: ...
    def set(self, key: str, value: str) -> None: ...


def new_vnc_password() -> str:
    """A fresh VNC password (VNC uses 8 characters), which x11vnc reads on the next connect."""
    password = "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(8))
    Path(os.environ["VNC_PASSWORD_FILE"]).write_text(password + "\n")
    return password


class Bridge:
    def __init__(
        self, state: State, emit: Callable[[botler.Event], object], new_password: Callable[[], str]
    ) -> None:
        self.state, self.emit, self.new_password = state, emit, new_password
        self.seen: list[str] = json.loads(state.get("seen") or "[]")
        # After a restart the VNC password is new, so a logged-out session is announced again.
        self.logged_in: bool | None = True if state.get("logged_in") == "1" else None

    def items(self, raw: object) -> None:
        """Emit each valid, new item of a page result (a list of {id, text, url})."""
        if not isinstance(raw, list):
            botler.log("page result is not a list; dropped", got=type(raw).__name__)
            return
        for entry in raw:  # pyright: ignore[reportUnknownVariableType]
            try:
                item = ExamplePageItem.model_validate(entry)
            except ValidationError as e:
                botler.log("invalid page item dropped", error=str(e))
                continue
            if item.id in self.seen:
                continue
            self.emit(item)
            self.seen = [*self.seen, item.id][-SEEN_LIMIT:]
            self.state.set("seen", json.dumps(self.seen))

    def pushed(self, events: list[dict[str, Any]]) -> None:
        """Items the page sent with `botlerSignal(json)`."""
        for ev in events:
            params = ev.get("params", {})
            if ev.get("method") != "Runtime.bindingCalled" or params.get("name") != "botlerSignal":
                continue
            try:
                self.items([json.loads(params["payload"])])
            except ValueError as e:
                botler.log("botlerSignal payload is not JSON; dropped", error=str(e))

    def check_login(self, logged_in: bool) -> None:
        if logged_in == self.logged_in:
            return
        password = self.new_password()
        if logged_in:
            self.emit(ExampleLoggedIn(site=config.SITE))
        else:
            self.emit(ExampleLoginNeeded(site=config.SITE, vnc_password=password))
        self.logged_in = logged_in
        self.state.set("logged_in", "1" if logged_in else "0")


def main() -> None:
    cdp = Cdp.open(page_ws_url("http://127.0.0.1:9223"))
    cdp.call("Runtime.enable")
    cdp.call("Runtime.addBinding", name="botlerSignal")
    if config.PUSH_JS:
        cdp.call("Page.addScriptToEvaluateOnNewDocument", source=config.PUSH_JS)
    cdp.call("Page.navigate", url=config.START_URL)
    bridge = Bridge(botler.state, botler.emit, new_vnc_password)
    while True:
        bridge.pushed(cdp.wait(config.POLL_SECONDS))
        if config.POLL_JS:
            bridge.items(cdp.evaluate(config.POLL_JS))
        bridge.check_login(cdp.evaluate(config.LOGGED_IN_JS) is True)
        botler.health.ok()


if __name__ == "__main__":
    main()
