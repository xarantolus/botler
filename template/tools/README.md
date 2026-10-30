# tools/

Python tools that agents can call. Each subdirectory is a uv workspace package whose functions are
decorated with `@botler.tool` (see `example/`):

```python
import botler


@botler.tool(effects=["network"])
def weather(city: str) -> str:
    """Current weather for a city."""
    ...
```

The tool is named `<package>.<function>` (`weather.weather` here). Type hints (pydantic types and
models work) give its argument schema and the docstring its description.

`python -m botler.tools manifest` regenerates `tools.manifest.json`; commit it. The gate fails if it
is stale. Agents get a tool when their `agent.toml` lists a matching glob in `tools = [...]`; pi
calls it as `weather_weather`, running `python -m botler.tools call` from the applied main commit.
A tool that needs a `hub`-class secret must be an `@action` in `connectors/` instead: those only run
after the user approves.
