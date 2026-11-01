// The sidebar's order: the front door, then pinned chats, then those waiting for the user, then
// by when someone last wrote in them.

interface Ordered {
  name: string;
  pinned: boolean;
  last_message_at: string;
}

export function chatOrder<T extends Ordered>(chats: T[], frontDoor: string | null, waiting: Record<string, unknown>): T[] {
  return [...chats].sort((a, b) => {
    if (a.name === frontDoor) return -1;
    if (b.name === frontDoor) return 1;
    const pinned = Number(b.pinned) - Number(a.pinned);
    const wait = Number(!!waiting[b.name]) - Number(!!waiting[a.name]);
    return pinned || wait || b.last_message_at.localeCompare(a.last_message_at);
  });
}
