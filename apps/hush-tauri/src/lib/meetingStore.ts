import type { MeetingDto, MeetingMessageDto } from "./api/types";

/**
 * The Meetings rail's view of a meeting.
 *
 * A meeting is a local record in the vault, not a relay-side room: the daemon
 * owns the record and its message log, and this store only mirrors what it last
 * returned. Nothing is persisted here — the record lives in the encrypted log,
 * which is what makes it survive a restart.
 */
export interface MeetingMessage {
  id: string;
  outgoing: boolean;
  /** The sender's nickname as this vault knows it; absent for our own messages. */
  from?: string;
  body: string;
  createdAt: number;
}

export interface Meeting {
  id: string;
  name: string;
  members: string[];
  createdAt: number;
  messages: MeetingMessage[];
}

let meetings: Meeting[] = [];

export function clearMeetingStore(): void {
  meetings = [];
}

function toMessage(m: MeetingMessageDto): MeetingMessage {
  return {
    id: m.id,
    outgoing: m.outgoing,
    from: m.from ?? undefined,
    body: m.body,
    createdAt: m.ts_ms,
  };
}

function toMeeting(dto: MeetingDto): Meeting {
  return {
    id: dto.id,
    name: dto.name,
    members: [...dto.members],
    createdAt: dto.created_ms,
    messages: dto.messages.map(toMessage),
  };
}

/**
 * Everything the Meetings rail renders, reduced to a comparable string, so the
 * poll can skip a repaint that would steal the caret for nothing.
 */
function signature(list: Meeting[]): string {
  return list
    .map((m) => {
      const last = m.messages[m.messages.length - 1];
      return `${m.id}:${m.name}:${m.members.join(",")}:${m.messages.length}:${
        last?.id ?? ""
      }`;
    })
    .join("|");
}

/** Rebuild the meeting list from the daemon. Returns whether anything changed. */
export function loadMeetings(dtos: MeetingDto[]): boolean {
  const next = dtos.map(toMeeting);
  const before = signature(meetings);
  meetings = next;
  return before !== signature(next);
}

/** Meetings, newest first. */
export function listMeetings(): Meeting[] {
  return [...meetings].sort((a, b) => b.createdAt - a.createdAt);
}

export function meetingById(id: string): Meeting | undefined {
  return meetings.find((m) => m.id === id);
}

/** The one-line summary under a meeting's name in the list. */
export function meetingPreview(m: Meeting): string {
  const last = m.messages[m.messages.length - 1];
  if (!last) return "No messages yet";
  const who = last.outgoing ? "You" : last.from ?? "Someone";
  return `${who}: ${last.body.replace(/\s+/g, " ").trim()}`;
}

/** Members of a meeting that are not already in it, for the add-member picker. */
export function candidatesToAdd(meeting: Meeting, contactNicks: string[]): string[] {
  return contactNicks.filter((n) => !meeting.members.includes(n));
}
