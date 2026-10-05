import type { ChatMessage } from "./api/types";

export interface MessageGroup {
  outgoing: boolean;
  day: string;
  messages: ChatMessage[];
}

export function dayLabel(ts: number): string {
  const d = new Date(ts);
  const today = new Date();
  const y = new Date(today);
  y.setDate(y.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === y.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: d.getFullYear() !== today.getFullYear() ? "numeric" : undefined,
  });
}

export function groupMessages(messages: ChatMessage[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  for (const m of messages) {
    const day = dayLabel(m.createdAt);
    const last = groups[groups.length - 1];
    if (
      last &&
      last.outgoing === m.outgoing &&
      last.day === day &&
      last.messages.length > 0
    ) {
      last.messages.push(m);
    } else {
      groups.push({ outgoing: m.outgoing, day, messages: [m] });
    }
  }
  return groups;
}
