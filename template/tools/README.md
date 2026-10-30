# tools/

Python tools that agents can call. Each subdirectory is a uv workspace package whose functions are
decorated with `@tool` from the botler SDK:

```python
from botler.tools import tool

@tool(effects=["network"])
def weather(city: str) -> str:
    """Current weather for a city."""
    ...
```

The gate regenerates `tools.manifest.json` from these and fails if the committed manifest is stale.
Agents get a tool when their `agent.toml` lists a matching glob in `tools = [...]`.
A tool that needs a `hub`-class secret must be an `@action` in `connectors/` instead: those only run
after the user approves.
