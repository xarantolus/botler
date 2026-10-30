// A chat's transcript as AI SDK `UIMessage`s, the shape Nuxt UI's chat components render.
//
// This is the only module coupled to the AI SDK's message types (type imports only: `ai` is
// never bundled). It is pure: the session items from the hub, the dialogs they opened, the
// user's messages the hub still holds (queued) and what is streaming in become one list of messages:
//
// - the user typing           → a `user` message with a `text` part;
// - any other delivery        → a `system` message with a `data-envelope` part (an event card);
// - one agent turn            → one `assistant` message: per LLM step a `reasoning` part
//   (thinking), a `text` part and its tool calls as `dynamic-tool` parts, each paired with its
//   result; a call that opened a dialog becomes a `data-dialog` part (the card replaces the
//   call); a provider error a `data-error` part;
// - notes and compactions     → `system` messages with `data-note` / `data-compaction`;
// - queued messages           → `user` messages with `metadata.queued` (and their delivery id);
// - streaming text / thinking → appended to the running turn (or a new trailing message).

import type { ChatStatus, UIMessage } from "ai";
import type { Dialog, QueuedMessage, SessionItem } from "../generated";
import { envelopeOf, isUserMessage, type Envelope } from "./envelope";
import { linkedDialog } from "./events";

export interface MessageMeta {
  /** When the (last) item of the message happened (ISO), if known. */
  at: string | null;
  /** The delivery that put it into the session (the `?delivery=` jump target). */
  delivery?: string | null;
  /** Delivery time in the instance's local time, from the delivery header. */
  localTime?: string | null;
  /** After a long silence, e.g. `first message in 3 days`. */
  gap?: string | null;
  /** The model of the (last) step of an agent turn. */
  model?: string | null;
  /** What the agent turn cost, summed over its steps. */
  costUsd?: number | null;
  /** Sent by the user and held by the hub until the turn ends (`delivery` cancels it). */
  queued?: boolean;
  /** Still being written (the live tail). */
  live?: boolean;
}

export interface NoteData {
  from: string;
  provenance: string;
  kind: string;
  text: string;
  localTime: string | null;
  at: string | null;
}

export type TranscriptData = {
  envelope: { envelope: Envelope; at: string | null };
  dialog: Dialog;
  note: NoteData;
  compaction: { summary: string };
  error: { text: string };
};

export type TranscriptMessage = UIMessage<MessageMeta, TranscriptData>;
export type TranscriptPart = TranscriptMessage["parts"][number];
export type ToolPart = Extract<TranscriptPart, { type: "dynamic-tool" }>;

export interface TranscriptInput {
  items: SessionItem[];
  /** Dialogs to resolve the links of tool results against. */
  dialogs: Dialog[];
  /** The user's messages the hub holds until the agent's turn ends. */
  queued: QueuedMessage[];
  /** Assistant text and thinking streaming in. */
  streaming: { text: string; thinking: string };
  /** The agent is working on a turn. */
  busy: boolean;
}

export interface Transcript {
  messages: TranscriptMessage[];
  status: ChatStatus;
  /** Tool results linking a dialog that is not in `dialogs` (older than the recent list). */
  unresolvedLinks: number;
  /** The message showing each session entry (a search hit links an entry id). */
  messageOf: Record<string, string>;
}

type ToolResult = Extract<SessionItem, { kind: "tool_result" }>;

/** The part showing a tool call (with its result, when there is one). */
export function toolPart(
  call: { id: string | null; name: string; arguments: unknown } | null,
  result: ToolResult | null,
  fallbackId: string,
): ToolPart {
  const base = {
    type: "dynamic-tool" as const,
    toolName: call?.name ?? result?.tool ?? "tool",
    toolCallId: call?.id ?? result?.tool_call_id ?? fallbackId,
    input: call?.arguments ?? null,
  };
  if (!result) return { ...base, state: "input-available" };
  if (result.is_error) return { ...base, state: "output-error", errorText: result.text };
  return { ...base, state: "output-available", output: result.text };
}

export function buildTranscript(input: TranscriptInput): Transcript {
  const { items, dialogs } = input;

  const results = new Map<string, ToolResult>();
  for (const it of items) if (it.kind === "tool_result" && it.tool_call_id) results.set(it.tool_call_id, it);
  /** Results shown with their call (not on their own). */
  const paired = new Set<string>();
  for (const it of items) {
    if (it.kind !== "assistant") continue;
    for (const c of it.tool_calls) {
      const r = c.id ? results.get(c.id) : undefined;
      if (r) paired.add(r.id);
    }
  }

  const messages: TranscriptMessage[] = [];
  let unresolvedLinks = 0;
  const messageOf: Record<string, string> = {};
  /** The agent turn being collected (consecutive assistant items and their results). */
  let turn: TranscriptMessage | null = null;

  const endTurn = () => {
    turn = null;
  };
  const currentTurn = (it: SessionItem): TranscriptMessage => {
    if (!turn) {
      turn = { id: it.id, role: "assistant", parts: [], metadata: { at: it.at, model: null, costUsd: null } };
      messages.push(turn);
    }
    return turn;
  };
  /** The part for a tool result: its dialog's card when it opened one, else the call. */
  const resultParts = (call: Parameters<typeof toolPart>[0], r: ToolResult | null, id: string): TranscriptPart[] => {
    if (r && (r.dialog_id || r.proposal_id)) {
      const d = linkedDialog(r, dialogs);
      if (d) return [{ type: "data-dialog", id: d.id, data: d }];
      unresolvedLinks++;
    }
    return [toolPart(call, r, id)];
  };

  for (const it of items) {
    switch (it.kind) {
      case "user": {
        endTurn();
        const env = envelopeOf(it);
        if (env && !isUserMessage(env)) {
          messages.push({
            id: it.id,
            role: "system",
            parts: [{ type: "data-envelope", data: { envelope: env, at: it.at } }],
            metadata: { at: it.at, delivery: it.delivery ?? env.delivery, localTime: env.localTime, gap: env.gap },
          });
        } else {
          messages.push({
            id: it.id,
            role: "user",
            parts: [{ type: "text", text: (it.message_text ?? env?.body ?? it.text).trim() }],
            metadata: {
              at: it.at,
              delivery: it.delivery ?? env?.delivery ?? null,
              localTime: it.local_time ?? env?.localTime ?? null,
              gap: it.gap ?? env?.gap ?? null,
            },
          });
        }
        break;
      }
      case "assistant": {
        const m = currentTurn(it);
        const meta = m.metadata!;
        meta.at = it.at ?? meta.at;
        meta.model = it.model ?? meta.model;
        if (it.cost_usd !== null) meta.costUsd = (meta.costUsd ?? 0) + it.cost_usd;
        if (it.thinking.trim()) m.parts.push({ type: "reasoning", text: it.thinking, state: "done" });
        if (it.text.trim()) m.parts.push({ type: "text", text: it.text, state: "done" });
        it.tool_calls.forEach((c, i) => {
          const r = c.id ? (results.get(c.id) ?? null) : null;
          if (r) messageOf[r.id] = m.id;
          m.parts.push(...resultParts(c, r, `${it.id}:${i}`));
        });
        if (it.error) m.parts.push({ type: "data-error", data: { text: it.error } });
        break;
      }
      case "tool_result": {
        if (paired.has(it.id)) break;
        // A result without its call (e.g. the call was compacted away).
        currentTurn(it).parts.push(...resultParts(null, it, it.id));
        break;
      }
      case "note":
        endTurn();
        messages.push({
          id: it.id,
          role: "system",
          parts: [
            {
              type: "data-note",
              data: {
                from: it.from,
                provenance: it.provenance,
                kind: it.event_kind,
                text: it.text.trim(),
                localTime: it.local_time,
                at: it.at,
              },
            },
          ],
          metadata: { at: it.at, delivery: it.delivery, localTime: it.local_time, gap: it.gap },
        });
        break;
      case "compaction":
        endTurn();
        messages.push({
          id: it.id,
          role: "system",
          parts: [{ type: "data-compaction", data: { summary: it.summary } }],
          metadata: { at: it.at },
        });
        break;
    }
    // Every kind adds to the last message, except a result shown with its call (mapped above).
    if (!paired.has(it.id)) messageOf[it.id] = messages.at(-1)!.id;
  }

  // The live tail: continue the running turn, or start one.
  const { text, thinking } = input.streaming;
  const streaming = text !== "" || thinking !== "";
  if (streaming) {
    const last = messages.at(-1);
    let m: TranscriptMessage;
    if (input.busy && last?.role === "assistant") {
      m = last;
    } else {
      m = { id: "live", role: "assistant", parts: [], metadata: { at: null } };
      messages.push(m);
    }
    m.metadata = { ...m.metadata!, live: true };
    if (thinking) m.parts.push({ type: "reasoning", text: thinking, state: text ? "done" : "streaming" });
    if (text) m.parts.push({ type: "text", text, state: "streaming" });
  }

  for (const q of input.queued) {
    messages.push({
      id: `queued:${q.delivery}`,
      role: "user",
      parts: [{ type: "text", text: q.text }],
      metadata: { at: q.at, queued: true, delivery: q.delivery },
    });
  }

  const status: ChatStatus = streaming ? "streaming" : input.busy ? "submitted" : "ready";
  return { messages, status, unresolvedLinks, messageOf };
}

/** Whether a tool call is still running (no result yet, while the agent works). */
export function toolRunning(part: ToolPart, busy: boolean): boolean {
  return busy && (part.state === "input-available" || part.state === "input-streaming");
}
