"""An example tool. An agent whose `agent.toml` grants `tools = ["example.*"]` can call it."""

import botler


@botler.tool(effects=[])
def word_count(text: str) -> int:
    """Count the words in a text."""
    return len(text.split())
