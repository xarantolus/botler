"""Daily live check of the web tools. Exits non-zero with what broke, which opens an incident for
`triage`; the fix belongs in tools/web/ (see its module docstrings). A single search backend failing
is only a warning, as long as `auto` (which falls back between them) still works."""

import sys
from collections.abc import Callable

from web.fetch import fetch
from web.search import BackendChoice, search

QUERY = "python programming language"


def check_search(backend: BackendChoice) -> Callable[[], str]:
    def check() -> str:
        r = search(QUERY, max_results=5, backend=backend)
        if len(r.results) < 3:
            raise AssertionError(f"expected at least 3 results, got {len(r.results)}: {r.results}")
        cost = f", ${r.cost_usd:.4f}" if r.cost_usd is not None else ""
        note = f" (note: {r.note})" if r.note else ""
        return f"web.search {backend} ok via {r.backend}: {len(r.results)} results{cost}{note}"

    return check


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


# (name, check, required): a failed optional check prints a warning and does not fail the job.
Check = tuple[str, Callable[[], str], bool]


def main(checks: list[Check]) -> int:
    failures: list[str] = []
    for name, check, required in checks:
        try:
            print(check())
        except Exception as e:  # every failure is collected and reported below
            line = f"- {name}: {type(e).__name__}: {e}"
            if required:
                failures.append(line)
            else:
                print(f"WARNING (not failing the job, another backend covers it):\n{line}")
    if failures:
        print(
            "web tools health check FAILED:\n"
            + "\n".join(failures)
            + "\nThe tools are monorepo code in tools/web/src/web/ (search.py, openrouter.py, fetch.py). "
            "Reproduce with `uv run python -m web_health`, fix them on a branch with an updated fixture "
            "and tests, and propose_change.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(
        main(
            [
                ("web.search openrouter", check_search("openrouter"), False),
                ("web.search duckduckgo", check_search("duckduckgo"), False),
                ("web.search auto", check_search("auto"), True),
                ("web.fetch", check_fetch, True),
            ]
        )
    )
