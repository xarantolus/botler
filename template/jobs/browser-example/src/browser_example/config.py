"""What the bridge watches. Adapt this per site; the rest of the bridge rarely changes.

All JavaScript here runs in the page. It never sees the botler token or any secret: the bridge
holds those, and the page only returns values or calls `botlerSignal(json)`.
"""

SITE = "example.com"
# Opened when the service starts; with the profile in /data the session is still logged in.
START_URL = "https://example.com/"

# The login check: an expression that is true only while logged in (an element only a logged-in
# page has). Checked every POLL_SECONDS.
LOGGED_IN_JS = "document.querySelector('h1') !== null"

# Poll: an expression returning a list of {"id", "text", "url"}; None to only use push.
POLL_JS: str | None = """
Array.from(document.querySelectorAll('a')).map(a => ({id: a.href, text: a.textContent.trim(), url: a.href}))
"""

# Push: injected into every page load; calls botlerSignal(JSON.stringify({id, text, url})) when
# something appears. None to only poll.
PUSH_JS: str | None = None

POLL_SECONDS = 60.0
