// A `todo` checklist for multi-step work, adapted from pi's examples/extensions/todo.ts without
// its TUI parts (agents run headless). The list lives in the session itself: every result
// carries the whole list in `details`, and it is rebuilt from the branch on start, so it
// survives sleeping and follows branches.

import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

interface Todo {
  id: number;
  text: string;
  done: boolean;
}

interface TodoDetails {
  todos: Todo[];
  nextId: number;
}

const TodoParams = Type.Object({
  action: StringEnum(["list", "add", "toggle", "clear"] as const),
  text: Type.Optional(Type.String({ description: "Todo text (for add)" })),
  id: Type.Optional(Type.Number({ description: "Todo ID (for toggle)" })),
});

export default function (pi: ExtensionAPI) {
  let todos: Todo[] = [];
  let nextId = 1;

  // The last successful `todo` result on the branch holds the current list.
  const reconstruct = (ctx: ExtensionContext) => {
    todos = [];
    nextId = 1;
    for (const entry of ctx.sessionManager.getBranch()) {
      if (entry.type !== "message") continue;
      const msg = entry.message;
      if (msg.role !== "toolResult" || msg.toolName !== "todo") continue;
      const details = msg.details as TodoDetails | undefined;
      if (details) {
        todos = details.todos.map((t) => ({ ...t })); // never mutate the recorded entry
        nextId = details.nextId;
      }
    }
  };
  pi.on("session_start", async (_event, ctx) => reconstruct(ctx));
  pi.on("session_tree", async (_event, ctx) => reconstruct(ctx));

  const result = (text: string) => ({
    content: [{ type: "text" as const, text }],
    details: { todos: todos.map((t) => ({ ...t })), nextId } satisfies TodoDetails,
  });

  pi.registerTool({
    name: "todo",
    label: "Todo",
    description:
      "Your checklist for multi-step work; it survives sleeping. Actions: list, add (text), toggle (id), clear",
    parameters: TodoParams,
    async execute(_toolCallId, params) {
      switch (params.action) {
        case "list":
          return result(
            todos.length ? todos.map((t) => `[${t.done ? "x" : " "}] #${t.id}: ${t.text}`).join("\n") : "No todos",
          );
        case "add": {
          if (!params.text) throw new Error("text is required for add");
          const todo: Todo = { id: nextId++, text: params.text, done: false };
          todos.push(todo);
          return result(`Added todo #${todo.id}: ${todo.text}`);
        }
        case "toggle": {
          if (params.id === undefined) throw new Error("id is required for toggle");
          const todo = todos.find((t) => t.id === params.id);
          if (!todo) throw new Error(`todo #${params.id} not found`);
          todo.done = !todo.done;
          return result(`Todo #${todo.id} ${todo.done ? "completed" : "uncompleted"}`);
        }
        case "clear": {
          const count = todos.length;
          todos = [];
          nextId = 1;
          return result(`Cleared ${count} todos`);
        }
      }
    },
  });
}
