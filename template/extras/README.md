# extras

Integrations that are ready to use but off until you want them: they need an account or a token
only you can create. They are workspace members, so the gate lints and tests them like everything
else, but the hub runs nothing from here.

To turn one on, ask botler, e.g. "set up the Discord channel". It reads the extra's README, moves
it to `jobs/<name>`, wires it up (for example `mcp = ["discord"]` on the agents that should reach
it), proposes the change, and asks you for its secret.

| extra | what it is |
|---|---|
| `discord` | A Discord bot as a channel: talk to botler in DMs, `/remind`, a "Send to Botler" message command, and `send_dm` so agents can message you unprompted. Secret `discord`. |
