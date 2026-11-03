"""The browser image's launcher must not re-introduce the usual headless/automation tells.

Reads the launcher and Dockerfile as text (no image build needed), so the gate stays offline.
Comments are stripped first: they may name the flags the launcher must avoid.
"""

from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
SESSION = (REPO / "images/browser/browser-session").read_text()
DOCKERFILE = (REPO / "images/browser/Dockerfile").read_text()
# Only the executable lines matter; the comments explain which flags are deliberately absent.
COMMAND = "\n".join(line for line in SESSION.splitlines() if not line.lstrip().startswith("#"))


def test_no_automation_flag_and_webdriver_is_disabled() -> None:
    assert "--enable-automation" not in COMMAND, "this flag sets navigator.webdriver"
    assert "--disable-blink-features=AutomationControlled" in COMMAND
    assert " --headless" not in COMMAND


def test_desktop_ua_window_and_password_store() -> None:
    assert "--user-agent=Mozilla/5.0 (X11; Linux x86_64)" in COMMAND
    assert "Chrome/${version}" in COMMAND, "the UA version must come from the image's Chromium"
    assert 'set -- "$@" "--user-agent=' in COMMAND, "the spaced UA must stay one argument"
    assert "--window-size=1920,1080" in COMMAND
    assert "--password-store=basic" in COMMAND


def test_swiftshader_webgl_and_fonts() -> None:
    # The container has no GPU; WebGL must still be present (SwiftShader) or it stands out.
    assert "--use-angle=swiftshader" in COMMAND and "--enable-unsafe-swiftshader" in COMMAND
    assert "font" in DOCKERFILE.lower()


def test_blocklist_step_and_opt_out() -> None:
    assert "/opt/blocklist/hosts" in COMMAND and "/etc/hosts" in COMMAND
    assert "${BROWSER_ADBLOCK:-1}" in COMMAND, "BROWSER_ADBLOCK=0 must skip the hosts step"
    assert "grep -q '^# botler blocklist' /etc/hosts" in COMMAND, "the step must be idempotent"
    assert "BLOCKLIST_COMMIT" in DOCKERFILE and "BLOCKLIST_SHA256" in DOCKERFILE
    assert "sha256sum -c" in DOCKERFILE
