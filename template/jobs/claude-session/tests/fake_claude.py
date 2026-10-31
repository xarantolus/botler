"""A stand-in for `claude -p --input-format stream-json`, as the spike saw the real one behave:
a message that arrives during a turn joins it; an interrupt ends the turn with
`error_during_execution`; closing stdin lets the running turn finish, then it exits. A message
`slow <seconds>` makes a turn take that long; `crash` exits at once."""

import json
import os
import select
import sys
import time

sid = sys.argv[sys.argv.index("--session-id" if "--session-id" in sys.argv else "--resume") + 1]
with open(os.environ["FAKE_CLAUDE_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps(sys.argv[1:]) + "\n")


def say(**msg: object) -> None:
    print(json.dumps({"session_id": sid, **msg}), flush=True)


say(type="system", subtype="init")
buf, until, eof = b"", 0.0, False
turn: list[str] = []
while not (eof and not turn):
    wait = max(0.0, until - time.monotonic()) if turn else None
    if eof:
        time.sleep(wait or 0)
    elif select.select([0], [], [], wait)[0]:
        chunk = os.read(0, 65536)
        eof = not chunk
        buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        msg = json.loads(line)
        if msg["type"] == "control_request":
            say(type="control_response", response={"subtype": "success"})
            if turn:
                say(type="result", subtype="error_during_execution", is_error=True)
                turn = []
            continue
        text = msg["message"]["content"]
        if text == "crash":
            print("boom", file=sys.stderr)
            sys.exit(3)
        if not turn:
            until = time.monotonic() + (float(text.split()[1]) if text.startswith("slow") else 0)
        turn.append(text)
    if turn and time.monotonic() >= until:
        say(type="result", subtype="success", is_error=False, result="did: " + " + ".join(turn))
        turn = []
