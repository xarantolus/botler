import { describe, expect, it } from "vitest";
import { chatOrder } from "./chatOrder";

const chat = (name: string, at: string, pinned = false) => ({ name, pinned, last_message_at: at });

describe("chatOrder", () => {
  it("puts botler first, then pinned, then waiting, then the latest written to", () => {
    const chats = [
      chat("old", "2026-11-01T08:00:00Z"),
      chat("new", "2026-11-01T10:00:00Z"),
      chat("asks", "2026-11-01T07:00:00Z"),
      chat("starred", "2026-10-30T00:00:00Z", true),
      chat("botler", "2026-10-01T00:00:00Z"),
    ];
    expect(chatOrder(chats, "botler", { asks: 1 }).map((c) => c.name)).toEqual(["botler", "starred", "asks", "new", "old"]);
  });
});
