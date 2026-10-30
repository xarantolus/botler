"""Python tools for pi agents.

    @botler.tool(effects=["network"])
    def weather(city: str) -> str:
        \"\"\"Current weather for a city.\"\"\"

The argument schema comes from the type hints (pydantic), the description from the docstring.
`python -m botler.tools manifest` writes `tools.manifest.json`; the gate checks it is current.
`python -m botler.tools call <name>` reads JSON arguments on stdin and writes
`{"ok": true, "result": ...}` or `{"ok": false, "error": {"type", "message"}}` on stdout.

Needs pydantic.
"""

from __future__ import annotations

import asyncio
import importlib
import inspect
import json
import sys
import traceback
from collections.abc import Callable
from pathlib import Path
from types import UnionType
from typing import Any, Literal, TypeVar, Union, get_args, get_origin, get_type_hints

import pydantic
import pydantic_core

from ._discover import import_all, manifest_parser, write_manifest

F = TypeVar("F", bound=Callable[..., Any])

_ATTR = "__botler_tool__"
_REGISTRY: dict[str, Callable[..., Any]] = {}


# Render hints: how the web app shows a tool's result (`ToolRender` in the hub, which checks the
# manifest against the same shapes). Field names are keys of the result; `items` names the key
# holding the list (the result itself when unset).
class _Hint(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="forbid", frozen=True)


class Links(_Hint):
    """Link cards: the `title` linking to the `url`, with a `snippet`."""

    kind: Literal["links"] = "links"
    items: str | None = None
    title: str | None = None
    url: str | None = None
    snippet: str | None = None


class Table(_Hint):
    """A table of the list's objects with these `columns` (all keys when unset)."""

    kind: Literal["table"] = "table"
    items: str | None = None
    columns: list[str] | None = None


class Markdown(_Hint):
    """Sanitized markdown: the result string, or its `field`."""

    kind: Literal["markdown"] = "markdown"
    field: str | None = None


class KeyValue(_Hint):
    """The result object's scalar fields as a key/value list."""

    kind: Literal["keyvalue"] = "keyvalue"


class Image(_Hint):
    """An image: the result (a URL, path or data URI), or its `field`."""

    kind: Literal["image"] = "image"
    field: str | None = None


class Json(_Hint):
    """Collapsible JSON."""

    kind: Literal["json"] = "json"


Render = Links | Table | Markdown | KeyValue | Image | Json


def tool(*, name: str | None = None, effects: list[str] | None = None, render: Render | None = None) -> Callable[[F], F]:
    """Declare a function as a pi tool. `name` defaults to `<top-level package>.<function>`, which
    agent definitions grant with globs (`tools = ["weather.*"]`). `effects` lists side effects for
    the reviewer, e.g. `network`, `fs_write`, `secret:GITLAB_TOKEN`. `render` tells the web app how
    to show the result, e.g. `Links(items="results")`; it guesses from the result's shape without."""
    if render is not None and not isinstance(render, _Hint):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise TypeError(f"render must be one of Links, Table, Markdown, KeyValue, Image, Json, not {render!r}")

    def wrap(fn: F) -> F:
        full = name or f"{fn.__module__.split('.')[0]}.{fn.__name__}"
        if full in _REGISTRY and _REGISTRY[full] is not fn:
            raise ValueError(f"tool {full} is declared twice")
        setattr(fn, _ATTR, {"name": full, "effects": list(effects or []), "render": render})
        _REGISTRY[full] = fn
        return fn

    return wrap


def _args_model(fn: Callable[..., Any]) -> type[pydantic.BaseModel]:
    hints = get_type_hints(fn, include_extras=True)
    fields: dict[str, Any] = {}
    for p in inspect.signature(fn).parameters.values():
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            raise TypeError(f"tool {fn.__qualname__}: *args/**kwargs cannot be described as a schema")
        if p.name not in hints:
            raise TypeError(f"tool {fn.__qualname__}: parameter {p.name} needs a type hint")
        default = ... if p.default is p.empty else p.default
        fields[p.name] = (hints[p.name], default)
    return pydantic.create_model(fn.__name__, __config__=pydantic.ConfigDict(extra="forbid"), **fields)


def _model(t: Any) -> type[pydantic.BaseModel] | None:
    return t if inspect.isclass(t) and issubclass(t, pydantic.BaseModel) else None


def _list_item(t: Any) -> Any:
    """`X` of `list[X]` (also inside `| None`), else None."""
    if get_origin(t) in (Union, UnionType):
        t = next((a for a in get_args(t) if a is not type(None)), None)
    return get_args(t)[0] if get_origin(t) is list and get_args(t) else None


def _check_render(full: str, fn: Callable[..., Any], render: Render) -> None:
    """The fields a hint names must exist when the tool returns a pydantic model (or a list of them)."""

    def need(model: type[pydantic.BaseModel] | None, names: list[str | None]) -> None:
        missing = [n for n in names if n is not None and model is not None and n not in model.model_fields]
        if model is not None and missing:
            raise ValueError(
                f"tool {full}: render {render.kind} names {', '.join(missing)}, which "
                f"{model.__name__} does not have (fields: {', '.join(model.model_fields)})"
            )

    ret = get_type_hints(fn).get("return")
    if isinstance(render, Links | Table):
        top = _model(ret)
        need(top, [render.items])
        holder = top.model_fields[render.items].annotation if top and render.items else ret
        item = _model(_list_item(holder))
        need(item, [render.title, render.url, render.snippet] if isinstance(render, Links) else [*(render.columns or [])])
    elif isinstance(render, Markdown | Image):
        need(_model(ret), [render.field])


def manifest() -> list[dict[str, Any]]:
    """Every registered tool in the format of `tools.manifest.json`."""
    out: list[dict[str, Any]] = []
    for full in sorted(_REGISTRY):
        fn = _REGISTRY[full]
        meta = getattr(fn, _ATTR)
        if "." in fn.__qualname__:
            raise TypeError(f"tool {full} must be a module-level function")
        doc = inspect.getdoc(fn)
        if not doc:
            raise ValueError(f"tool {full} needs a docstring: it is the description the model reads")
        schema = _args_model(fn).model_json_schema()
        schema.pop("title", None)
        entry: dict[str, Any] = {
            "name": full,
            "description": doc,
            "parameters": schema,
            "effects": meta["effects"],
            "approval": False,
            "module": fn.__module__,
            "function": fn.__name__,
        }
        render: Render | None = meta["render"]
        if render is not None:
            _check_render(full, fn, render)
            entry["render"] = render.model_dump(exclude_none=True)
        out.append(entry)
    return out


def run(entry: dict[str, Any], args: Any) -> Any:
    """Validate `args` against the tool's signature, call it, and return a JSON-able result."""
    fn = getattr(importlib.import_module(entry["module"]), entry["function"])
    model = _args_model(fn).model_validate(args if args is not None else {})
    result = fn(**{k: getattr(model, k) for k in type(model).model_fields})
    if inspect.isawaitable(result):
        result = asyncio.run(_await(result))
    return pydantic_core.to_jsonable_python(result)


async def _await(x: Any) -> Any:
    return await x


def _call(name: str, manifest_path: Path) -> int:
    try:
        entries = {e["name"]: e for e in json.loads(manifest_path.read_text())}
        if name not in entries:
            raise LookupError(f"no tool {name} in {manifest_path}")
        result = run(entries[name], json.loads(sys.stdin.read() or "null"))
    except Exception as e:  # reported to the caller, never dropped
        traceback.print_exc(file=sys.stderr)
        error: dict[str, Any] = {"type": type(e).__name__, "message": str(e)}
        code = getattr(e, "code", None)
        if isinstance(code, str):
            error["code"] = code
        print(json.dumps({"ok": False, "error": error}))
        return 1
    print(json.dumps({"ok": True, "result": result}))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser, sub = manifest_parser("botler.tools", "tools.manifest.json")
    c = sub.add_parser("call", help="run a tool: JSON arguments on stdin, JSON result on stdout")
    c.add_argument("name")
    c.add_argument("--manifest", type=Path, default=Path("tools.manifest.json"))
    args = parser.parse_args(argv)
    if args.command == "call":
        return _call(args.name, args.manifest)
    import_all(args.root)
    return write_manifest("botler.tools", args.root / "tools.manifest.json", manifest(), args.check)


if __name__ == "__main__":
    # Run the importable module's main so user code and this command share one registry.
    from botler.tools import main as _main

    sys.exit(_main())
