/**
 * Slash-command parsing and dispatch.
 *
 * Commands are parsed here, on the frontend, so `/help` and errors print
 * instantly and locally. Anything that is just a raw IRC line (`/whois`,
 * `/mode`, `/kick`, ...) is forwarded through `send_raw`; nothing unknown is
 * ever sent to the server.
 */

import {
  certAuthorize,
  certForget,
  certGenerate,
  certStatus,
  changeNick,
  clearWindowLines,
  connectServer,
  disconnectServer,
  joinChannel,
  openQuery,
  partChannel,
  pushLocalLine,
  pushLocalStatus,
  requestChannelList,
  sendMessage,
  sendRaw,
} from "./ircStore";

export interface SlashContext {
  serverId: string;
  target: string;
  isChannel: boolean;
  /** The channel a channel-only command should act on (target when it is one). */
  channel: string | null;
}

const HELP_LINES = [
  "Available commands:",
  "/join #chan [key]        join a channel",
  "/part [#chan] [message]  leave a channel",
  "/msg <nick> <text>       send a private message",
  "/query <nick>            open a private message window",
  "/me <action>             send an action",
  "/nick <newnick>          change your nickname",
  "/topic [text]            show or set the channel topic",
  "/whois <nick>            look up a user",
  "/list                    open the channel list",
  "/away [message]          set or clear your away status",
  "/mode <target> <modes>   set modes",
  "/kick <nick> [reason]    kick a user from the channel",
  "/ban [nick]              ban a user (no nick: list bans)",
  "/op /deop /voice /devoice <nick>",
  "/server <host> [port]    open another connection",
  "/cert                    show this device's certificate fingerprint",
  "/cert authorize          register the fingerprint with NickServ",
  "/cert forget             delete the certificate from the vault",
  "/clear                   clear this window",
  "/raw <line>              send a raw IRC line",
  "/quit [message]          disconnect",
  "/help                    show this list",
];

function usage(serverId: string, target: string, text: string): void {
  pushLocalLine(serverId, target, "error", text);
}

function fail(serverId: string, target: string, err: unknown): void {
  const message = err instanceof Error ? err.message : String(err);
  pushLocalLine(serverId, target, "error", message);
}

/** Whether the input is a slash command (so the caller sends it as a message otherwise). */
export function isSlashCommand(input: string): boolean {
  return input.trimStart().startsWith("/");
}

/**
 * Run a slash command. Returns true when it was recognized and handled; false
 * means the caller should treat the input as an ordinary message.
 */
export function runSlashCommand(input: string, ctx: SlashContext): boolean {
  const trimmed = input.trim();
  if (!trimmed.startsWith("/")) return false;
  const withoutSlash = trimmed.slice(1);
  const space = withoutSlash.indexOf(" ");
  const command = (space === -1 ? withoutSlash : withoutSlash.slice(0, space)).toLowerCase();
  const rest = space === -1 ? "" : withoutSlash.slice(space + 1).trim();
  const { serverId, target, channel } = ctx;

  const needChannel = (): string | null => {
    if (channel) return channel;
    usage(serverId, target, "This command only works in a channel window.");
    return null;
  };

  switch (command) {
    case "join": {
      if (!rest) {
        usage(serverId, target, "Usage: /join #channel [key]");
        return true;
      }
      const [chan, key] = splitFirst(rest);
      joinChannel(serverId, chan, key).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "part": {
      const [first, reason] = splitFirst(rest);
      const chan = first.startsWith("#") || first.startsWith("&") ? first : channel;
      if (!chan) {
        usage(serverId, target, "Usage: /part [#channel] [message]");
        return true;
      }
      const message = first.startsWith("#") || first.startsWith("&") ? reason : rest;
      partChannel(serverId, chan, message || undefined).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "msg": {
      const [nick, text] = splitFirst(rest);
      if (!nick || !text) {
        usage(serverId, target, "Usage: /msg <nick> <text>");
        return true;
      }
      openQuery(serverId, nick);
      sendMessage(serverId, nick, text).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "query": {
      if (!rest) {
        usage(serverId, target, "Usage: /query <nick>");
        return true;
      }
      openQuery(serverId, rest.split(/\s+/)[0]);
      return true;
    }
    case "me": {
      if (!rest) {
        usage(serverId, target, "Usage: /me <action>");
        return true;
      }
      sendMessage(serverId, target, rest, true).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "nick": {
      if (!rest) {
        usage(serverId, target, "Usage: /nick <newnick>");
        return true;
      }
      changeNick(serverId, rest.split(/\s+/)[0]).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "topic": {
      const chan = needChannel();
      if (!chan) return true;
      sendRaw(serverId, rest ? `TOPIC ${chan} :${rest}` : `TOPIC ${chan}`).catch((e) =>
        fail(serverId, target, e),
      );
      return true;
    }
    case "whois": {
      if (!rest) {
        usage(serverId, target, "Usage: /whois <nick>");
        return true;
      }
      sendRaw(serverId, `WHOIS ${rest.split(/\s+/)[0]}`).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "list": {
      requestChannelList(serverId);
      return true;
    }
    case "away": {
      sendRaw(serverId, rest ? `AWAY :${rest}` : "AWAY").catch((e) => fail(serverId, target, e));
      return true;
    }
    case "mode": {
      if (!rest) {
        usage(serverId, target, "Usage: /mode <target> <modes> [args]");
        return true;
      }
      sendRaw(serverId, `MODE ${rest}`).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "kick": {
      const chan = needChannel();
      if (!chan) return true;
      const [nick, reason] = splitFirst(rest);
      if (!nick) {
        usage(serverId, target, "Usage: /kick <nick> [reason]");
        return true;
      }
      const line = reason ? `KICK ${chan} ${nick} :${reason}` : `KICK ${chan} ${nick}`;
      sendRaw(serverId, line).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "ban": {
      const chan = needChannel();
      if (!chan) return true;
      const nick = rest.split(/\s+/)[0];
      sendRaw(serverId, nick ? `MODE ${chan} +b ${nick}` : `MODE ${chan} +b`).catch((e) =>
        fail(serverId, target, e),
      );
      return true;
    }
    case "op":
    case "deop":
    case "voice":
    case "devoice": {
      const chan = needChannel();
      if (!chan) return true;
      const nick = rest.split(/\s+/)[0];
      if (!nick) {
        usage(serverId, target, `Usage: /${command} <nick>`);
        return true;
      }
      const flag =
        command === "op" ? "+o" : command === "deop" ? "-o" : command === "voice" ? "+v" : "-v";
      sendRaw(serverId, `MODE ${chan} ${flag} ${nick}`).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "server": {
      const [host, portText] = splitFirst(rest);
      if (!host) {
        usage(serverId, target, "Usage: /server <host> [port]");
        return true;
      }
      const port = portText ? Number(portText) : 6697;
      const tls = port !== 6667;
      connectServer({
        host,
        port: Number.isFinite(port) ? port : 6697,
        tls,
        use_tor: false,
        nick: "",
      }).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "cert": {
      // SASL EXTERNAL authenticates with this device's certificate, and EXTERNAL
      // is the only mechanism Libera's Tor onion accepts. Authorizing it has to
      // happen over a connection that is already identified (the account's own
      // password), which is why this is a command against a live server rather
      // than something the connect dialog can do on its own.
      //
      // Every acknowledgement goes to the window the command was typed in. It
      // used to go to the server's status window, so `/cert authorize` in a
      // channel produced no visible output at all and read as a command that did
      // nothing. NickServ's own answer still arrives where the client puts a
      // service notice: its own query window, opened on arrival.
      const action = rest.split(/\s+/)[0].toLowerCase();
      if (action === "authorize") {
        certAuthorize(serverId)
          .then((fingerprint) =>
            pushLocalLine(
              serverId,
              target,
              "status",
              `Sent: PRIVMSG NickServ :CERT ADD ${fingerprint} — NickServ's reply opens a NickServ window.`,
            ),
          )
          .catch((e) => fail(serverId, target, e));
        return true;
      }
      if (action === "forget") {
        certForget()
          .then(() =>
            pushLocalLine(
              serverId,
              target,
              "status",
              "This device's certificate was deleted from the vault. NickServ still has it authorized until /msg NickServ CERT DEL <fingerprint>.",
            ),
          )
          .catch((e) => fail(serverId, target, e));
        return true;
      }
      if (action) {
        usage(serverId, target, "Usage: /cert [authorize|forget]");
        return true;
      }
      certStatus()
        .then((status) => {
          if (!status.exists || !status.fingerprint_sha512) {
            pushLocalLine(
              serverId,
              target,
              "status",
              "This device has no certificate. Generate one in the connect dialog before using SASL EXTERNAL.",
            );
            return;
          }
          pushLocalLine(
            serverId,
            target,
            "status",
            `This device's certificate fingerprint: ${status.fingerprint_sha512}`,
          );
          pushLocalLine(
            serverId,
            target,
            "status",
            "Authorize it with /cert authorize, from a connection that is already identified.",
          );
        })
        .catch((e) => fail(serverId, target, e));
      return true;
    }
    case "clear": {
      clearWindowLines(serverId, target);
      return true;
    }
    case "raw": {
      if (!rest) {
        usage(serverId, target, "Usage: /raw <line>");
        return true;
      }
      sendRaw(serverId, rest).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "quit": {
      // A graceful quit, not a raw `QUIT`: the raw form makes the server close
      // the socket, which the client would treat as a drop and reconnect.
      disconnectServer(serverId, rest || undefined).catch((e) => fail(serverId, target, e));
      return true;
    }
    case "help": {
      for (const line of HELP_LINES) {
        pushLocalStatus(serverId, line);
      }
      return true;
    }
    default:
      pushLocalLine(serverId, target, "error", `Unknown command: /${command} (try /help)`);
      return true;
  }
}

/** Split the first whitespace-delimited word from the rest. */
function splitFirst(input: string): [string, string] {
  const trimmed = input.trim();
  const space = trimmed.indexOf(" ");
  if (space === -1) return [trimmed, ""];
  return [trimmed.slice(0, space), trimmed.slice(space + 1).trim()];
}
