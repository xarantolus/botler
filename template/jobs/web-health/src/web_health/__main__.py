"""Daily live check of the web tools. Exits non-zero with what broke, which opens an incident for
`triage`; the fix belongs in tools/web/ (see its module docstrings)."""

import sys
from collections.abc import Callable

from web.fetch import fetch
from web.search import search


def check_search() -> str:
    results = search("python programming language", max_results=5)
    if len(results) < 3:
        raise AssertionError(f"expected at least 3 results, got {len(results)}: {results}")
    return f"web.search ok: {len(results)} results, first {results[0].url}"


def check_fetch() -> str:
    page = fetch("https://en.wikipedia.org/wiki/Python_(programming_language)", max_chars=200_000)
    if page.status != 200:
        raise AssertionError(f"expected HTTP 200, got {page.status} from {page.url}")
    if page.total_chars < 5000 or "Guido van Rossum" not in page.content:
        raise AssertionError(
            f"the article came back as {page.total_chars} characters without 'Guido van Rossum' "
            f"(note: {page.note}); markdown extraction is broken"
        )
    return f"web.fetch ok: {page.total_chars} characters of markdown"


def main(checks: list[tuple[str, Callable[[], str]]]) -> int:
    failures: list[str] = []
    for name, check in checks:
        try:
            print(check())
        except Exception as e:  # every failure is collected and reported below
            failures.append(f"- {name}: {type(e).__name__}: {e}")
    if failures:
        print(
            "web tools health check FAILED:\n"
            + "\n".join(failures)
            + "\nThe tools are monorepo code in tools/web/src/web/ (search.py, fetch.py). Reproduce with "
            "`uv run python -m web_health`, fix them on a branch with an updated fixture and tests, and "
            "propose_change.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main([("web.search", check_search), ("web.fetch", check_fetch)]))
