// The sidebar's order: the front door, then pinned chats, then those waiting for the user, then
// the busiest lately (the hub's activity score: messages in the last hours, decaying, helpers
// such as reviewers counting less), then by when someone last wrote in them.

interface Ordered {
  name: string;
  pinned: boolean;
  last_message_at: string;
}

export function chatOrder<T extends Ordered>(
  chats: T[],
  frontDoor: string | null,
  waiting: Record<string, unknown>,
  activity: Record<string, number> = {},
): T[] {
  return [...chats].sort((a, b) => {
    if (a.name === frontDoor) return -1;
    if (b.name === frontDoor) return 1;
    const pinned = Number(b.pinned) - Number(a.pinned);
    const wait = Number(!!waiting[b.name]) - Number(!!waiting[a.name]);
    const busy = (activity[b.name] ?? 0) - (activity[a.name] ?? 0);
    return pinned || wait || busy || b.last_message_at.localeCompare(a.last_message_at);
  });
}
