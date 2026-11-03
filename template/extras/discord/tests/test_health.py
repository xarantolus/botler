"""A hub restart must not end the channel service.

Incident 2026-11-01: while the hub was being restarted, `botler.health.ok()` raised `BotlerError`
(unreachable, after the SDK's built-in 90 s reconnect) and the process exited 1, even though
Discord itself was fine. `report_health` swallows exactly that error and logs it, so the
listener and the outbox keep running and re-attach when the hub is back.
"""

from __future__ import annotations

import botler
from discord_bot.__main__ import report_health


class FakeHealth:
    def __init__(self, error: Exception | None = None) -> None:
        self.calls: list[object] = []
        self.error = error

    def ok(self) -> None:
        self.calls.append("ok")
        if self.error is not None:
            raise self.error

    def fail(self, reason: str) -> None:
        self.calls.append(("fail", reason))
        if self.error is not None:
            raise self.error


def _log(*args: object, **kwargs: object) -> None:
    pass


def test_reports_a_healthy_service() -> None:
    health = FakeHealth()
    report_health(health, ok=True, log=_log)
    assert health.calls == ["ok"]


def test_reports_fail_with_its_reason() -> None:
    health = FakeHealth()
    reason = "not connected to Discord"
    report_health(health, ok=False, log=_log, reason=reason)
    assert health.calls == [("fail", reason)]


def test_a_hub_restart_is_logged_not_raised() -> None:
    error = botler.BotlerError("health", "unreachable", "cannot reach the hub at /run/botler/hub.sock")
    health = FakeHealth(error)
    logged: list[str] = []
    report_health(health, ok=True, log=lambda *a, **k: logged.append("reported"))
    assert health.calls == ["ok"]
    assert logged == ["reported"]
