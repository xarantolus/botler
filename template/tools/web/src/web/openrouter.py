"""The OpenRouter backend of `web.search`: OpenRouter's `web` plugin
(https://openrouter.ai/docs/guides/features/plugins/web-search) runs the search and returns the
results as `url_citation` annotations on the reply; the model's prose is ignored.

Cost per search: the Parallel engine in `turbo` mode costs $0.001 per request (up to 10 results,
$0.001 per further result), plus the prompt tokens of the results on the cheapest model below
(about $0.00002). `usage.cost` in the response is the exact amount and is returned with the results.
`tests/fixtures/openrouter_response.json` is a recorded response to test against.
"""

from __future__ import annotations

import os
from typing import Any

import httpx
from pydantic import BaseModel, Field, ValidationError

from . import _http
from ._http import WebToolError, fix_hint

URL = "https://openrouter.ai/api/v1/chat/completions"
KEY_VAR = "OPENROUTER_API_KEY"
# Non-reasoning and among the cheapest models; it only has to accept the injected results.
MODEL = "mistralai/mistral-nemo"
SNIPPET_CHARS = 400


def _error(code: str, what: str) -> WebToolError:
    return WebToolError(code, f"OpenRouter web search failed: {what}. {fix_hint('openrouter')}")


def search(query: str, max_results: int) -> tuple[list[dict[str, str]], float | None]:
    """([{title, url, snippet}], cost in USD or None) for `query`."""
    key = os.environ.get(KEY_VAR)
    if not key:
        raise WebToolError(
            "no_key",
            f"OpenRouter web search needs {KEY_VAR} in the environment. Agents get it with their model "
            'route (botler.toml, api_key_secret); a job needs `secrets = ["openrouter"]` in its job.toml.',
        )
    body = {
        "model": MODEL,
        "max_tokens": 16,
        "plugins": [{"id": "web", "engine": "parallel", "mode": "turbo", "max_results": max_results}],
        "messages": [{"role": "user", "content": query}],
    }
    try:
        with _http.client() as http:
            resp = http.post(URL, json=body, headers={"Authorization": f"Bearer {key}"}, timeout=60)
    except httpx.HTTPError as e:
        raise WebToolError("network", f"OpenRouter request failed: {type(e).__name__}: {e}") from e
    if resp.status_code != 200:
        hint = {
            401: f" {KEY_VAR} is invalid or revoked; the user replaces it in the web app (Secrets).",
            402: " The OpenRouter account is out of credits; the user tops it up at openrouter.ai.",
            429: " Rate limited; wait a minute before searching again.",
        }.get(resp.status_code, "")
        raise WebToolError("http", f"OpenRouter answered HTTP {resp.status_code}: {resp.text[:500]}.{hint}")
    return parse_response(resp.json())


class _Citation(BaseModel):
    url: str
    title: str | None = None
    content: str | None = None


class _Annotation(BaseModel):
    type: str
    url_citation: _Citation | None = None


class _Message(BaseModel):
    annotations: list[_Annotation] | None = None


class _Choice(BaseModel):
    message: _Message


class _Usage(BaseModel):
    cost: float | None = None


class _Response(BaseModel):
    choices: list[_Choice] = Field(min_length=1)
    usage: _Usage | None = None


def parse_response(data: Any) -> tuple[list[dict[str, str]], float | None]:  # noqa: ANN401 (JSON)
    if isinstance(data, dict) and "error" in data:
        raise _error("http", f"the response is an error: {data['error']}")
    try:
        response = _Response.model_validate(data)
    except ValidationError as e:
        raise _error("markup_changed", f"unexpected response shape: {e}") from e
    results: list[dict[str, str]] = []
    for a in response.choices[0].message.annotations or []:
        c = a.url_citation
        if c is None:
            continue
        snippet = " ".join((c.content or "").split())
        if len(snippet) > SNIPPET_CHARS:
            snippet = snippet[:SNIPPET_CHARS].rsplit(" ", 1)[0] + " ..."
        results.append({"title": c.title or c.url, "url": c.url, "snippet": snippet})
    return results, response.usage.cost if response.usage else None
