import { api, AppError, getApiBase } from "./lib/api/daemon";
import type {
  ContactDto,
  DropsKeyDto,
  StatusResponse,
  ReadsCommentDto,
  ReadsSourceDto,
  ReadsStateDto,
  ReadsStoryDetailDto,
  ReadsStoryDto,
  TorrentFileDto,
  TorrentItemDto,
  TorrentLibraryEntryDto,
  TorrentsStateDto,
  NotesStateDto,
  WalletChain,
  WalletChainDto,
  WalletNetworkMode,
  WalletStateDto,
  WalletTokenDto,
  ZinePendingCommentDto,
  ZineSummaryDto,
  ZineViewDto,
  ZinesViewDto,
} from "./lib/api/types";
import {
  HK_NEWS,
  addSource as addReadsSource,
  clearReadsStore,
  getState as getReadsState,
  getSources as getReadsSources,
  loadFeed as loadReadsFeed,
  loadReader as loadReadsReader,
  loadStory as loadReadsStory,
  markRead as markReadsRead,
  removeSource as removeReadsSource,
  timeAgo as readsTimeAgo,
  toggleHidden as toggleReadsHidden,
  toggleSaved as toggleReadsSaved,
} from "./lib/readsStore";
import {
  clearNotesStore,
  deleteNote as deleteNoteRecord,
  getNotesState,
  notePreview,
  peekNotesState,
  saveNote,
  searchNotes,
} from "./lib/notesStore";
import {
  addTorrent,
  addTorrentFile,
  clearTorrentsStore,
  deleteLibraryEntry,
  getTorrentsState,
  pauseTorrent,
  refreshTorboxAccount,
  refreshTorrents,
  removeTorrent,
  resumeTorrent,
  retryTorrent,
  saveTorboxKey,
  setTorrentConcurrency,
  setTorrentsHooks,
  setTorrentsPaneOpen,
  subscribeTorrents,
  testTorboxKey,
  torrentStatusLabel,
} from "./lib/torrentsStore";
import { showToast } from "./lib/toast";
import {
  CHAINS,
  DEFAULT_ETHEREUM_NETWORK,
  DEFAULT_FEE_RATE,
  addToken,
  chainState,
  clearWalletStore,
  createWallet,
  formatBase,
  formatBtc,
  formatEth,
  formatToken,
  formatUnits,
  getWalletLoadError,
  getWalletState,
  isWalletLoaded,
  newReceiveAddress,
  parseUnits,
  refreshWallet,
  removeToken,
  restoreWallet,
  sendPayment,
  setWalletMode,
  setWalletPaneOpen,
  setWalletProxy,
  subscribeWallet,
  syncWallet,
  wipeWallet,
} from "./lib/walletStore";
import {
  addOutgoingFile,
  addOutgoingText,
  contactsWithFiles,
  fileMessagesForContact,
  fileCountForContact,
  clearChatStore,
  loadHistory,
  getThread,
  hasUnread,
  ingestIncoming,
  lastPreview,
  markRead,
  removeOutgoingMessage,
  clearSendProgress,
  setMessageBlobId,
  setSendProgress,
  allFileMessages,
  toggleReaction,
  unreadTotal,
} from "./lib/chatStore";
import { MAX_FILE_BYTES, RECV_POLL_MS, STARTUP_UPDATE_DELAY_MS } from "./lib/constants";
import { invoke } from "@tauri-apps/api/core";
import { isTauriShell } from "./lib/tauriEnv";
import { playChatSound } from "./lib/chatSound";
import {
  checkForUpdates,
  discardPendingUpdate,
  downloadAndInstallUpdate,
  getCurrentVersion,
  type AvailableUpdate,
  type UpdateProgress,
} from "./lib/updater";
import {
  clearMailStore,
  draftById,
  draftPreview,
  folderCount,
  getDropById,
  getThread as getMailThread,
  hasUnreadMail,
  isDropUnread,
  isUnread,
  listDrafts,
  listDropInbox,
  listDropsInFolder,
  listFolder,
  loadMailbox,
  preview,
  previewText,
  searchMessages,
  setKnownContacts,
  threadSubject,
  unreadCount,
  type DropMessage,
  type MailAttachmentRef,
  type MailFlags,
  type MailFolder,
  type MailMessage,
} from "./lib/mailStore";
import {
  getAutoLockMs,
  getAutoLockMinutes,
  mailFlags,
  patchUiPrefs,
  setMailFlag,
  uiPrefsSnapshot,
} from "./lib/api/uiPrefs";
import { displayNick, getMockSettings, setNickOverride, clearMockLocal, chatSoundEnabled, isChatSoundMuted, setChatSoundMuted, setChatSoundEnabled, refreshContactPrefs } from "./lib/api/mock";
import { clearContactUiPrefs } from "./lib/api/uiPrefs";
import { fileToBase64, formatAbsolute, formatBytes, formatRelative, formatUntil } from "./lib/format";
import {
  attachFileActions,
  downloadBase64File,
  openVaultBlob,
  readFileAsBase64,
  resolveMessageFileBytes,
  saveVaultBlob,
  type FileBytesSource,
} from "./lib/fileBytes";
import { groupMessages, dayLabel } from "./lib/messageGroups";
import {
  candidatesToInvite,
  clearConclaveStore,
  conclaveById,
  conclavePreview,
  invitedTrustees,
  joinedTrustees,
  listConclaves,
  listPendingInvites,
  loadConclaves,
  loadPendingInvites,
  type Conclave,
} from "./lib/conclaveStore";
import {
  attachEditor,
  closePaper,
  createPaper,
  flushPaperEdits,
  openPaper,
  paperHasFocus,
  paperSession,
} from "./lib/paperStore";
import {
  attachZineEditor,
  closeZineEditor,
  parseSections,
  penNameLabel,
  shortId,
  zineEditorHasFocus,
  zineEditorHtml,
  zineEditorIsDirty,
} from "./lib/zineStore";
import type { ZinesSectionCount } from "./lib/zinesPage";
import {
  byByline,
  bylineOf,
  groupBySection,
  leadOf,
  sectionCounts,
} from "./lib/zinesPage";
import {
  dropById,
  verifiedCount,
  visibleDrops,
} from "./lib/drops/registry";
import {
  isOpenable,
  verificationDetail,
  verificationLabel,
} from "./lib/drops/verification";
import type { Drop } from "./lib/drops/types";
import {
  closeDrop,
  devEnabled,
  dropsAvailable,
  openDrop,
  openState,
  torStart,
  torStatus,
  torStop,
  type TorPhase,
  type TorStatus,
} from "./lib/drops/tauri";
import { hueFromKey } from "./lib/hash";
import { icon, type IconName } from "./lib/icons";
import { avatarWithPresence, createAvatarEl, initialsFor } from "./components/ui/Avatar";
import { RichTextEditor } from "./components/ui/RichTextEditor";
import { ContactPicker } from "./components/ui/ContactPicker";
import { OverflowMenu } from "./components/ui/OverflowMenu";
import { bodyToDisplayHtml, isBlankHtml } from "./lib/richText";
import { applyTheme, getThemeMode, setThemeMode, type ThemeMode } from "./lib/theme";
import { IrcPane } from "./features/irc/ircPane";
import {
  getIrcState,
  initIrcStore,
  ircMentionCount,
  ircUnreadTotal,
  subscribeIrc,
} from "./features/irc/ircStore";
import QRCode from "qrcode";

type Rail =
  | "chats"
  | "trusts"
  | "conclaves"
  | "zines"
  | "reads"
  | "torrents"
  | "irc"
  | "mail"
  | "drops"
  | "vault"
  | "settings";
type SettingsSection =
  | "appearance"
  | "profile"
  | "relay"
  | "drops"
  | "torbox"
  | "notifications"
  | "security"
  | "recovery"
  | "backup"
  | "about";

/**
 * One row in the Exchanged-files browser: a chat attachment or a mail one.
 * Both live as encrypted blobs in the vault, so both are browsed from the
 * Vault; `via` decides which pane the deep link returns to.
 */
interface ExchangedFileRow {
  name: string;
  size: number;
  at: number;
  via: "chat" | "mail";
  /** Bytes resolvable on this device, or null if the file is gone. */
  bytes: FileBytesSource | null;
  blobId: string | null;
  /** Mail only: the carrying message's subject, shown for context. */
  subject?: string;
  /** Mail only: the carrying message, for the View-in-mail deep link. */
  mail?: MailMessage;
}

/** Matches the daemon: at most this many attachments, and this many bytes total. */
const MAX_MAIL_ATTACHMENTS = 8;
const MAX_MAIL_BYTES = 256 * 1024;

/**
 * Matches `DEFAULT_VAULT_NAME` in `hush-core`: the suffix for a vault that has
 * no name of its own yet (or a contact whose invite predates vault naming).
 */
const DEFAULT_VAULT_NAME = "hush";

const MAIL_FOLDER_LABEL: Record<MailFolder, string> = {
  inbox: "Inbox",
  drops: "Drops",
  sent: "Sent",
  outbox: "Outbox",
  drafts: "Drafts",
  starred: "Starred",
  archive: "Archive",
  trash: "Trash",
};

const MAIL_EMPTY: Record<MailFolder, string> = {
  inbox: "No mail yet — write to a contact.",
  drops: "No drops yet. Anything sealed to your published drop key lands here.",
  sent: "Nothing sent yet.",
  outbox: "Nothing waiting. Mail the relay could not take would queue here.",
  drafts: "No drafts.",
  starred: "No starred mail.",
  archive: "Nothing archived.",
  trash: "Trash is empty.",
};

/** `Re:` a subject, without stacking prefixes on an existing reply. */
function reSubject(subject: string): string {
  return /^re:/i.test(subject.trim()) ? subject : `Re: ${subject}`;
}

function fwdSubject(subject: string): string {
  return /^fwd:/i.test(subject.trim()) ? subject : `Fwd: ${subject}`;
}

/** How many torrents still need the pipeline's attention (for the rail badge). */
function activeTorrentCount(state: TorrentsStateDto): number {
  return state.items.filter((i) =>
    ["queued", "sending", "caching", "ready", "downloading"].includes(i.status),
  ).length;
}

/** Whether a pasted string is a magnet link or a `.torrent` URL. Mirrors the
 * daemon's own check so the Add bar can reject the obvious typo locally. */
function looksLikeTorrentSource(s: string): boolean {
  const t = s.trim().toLowerCase();
  if (t.startsWith("magnet:")) return true;
  return (t.startsWith("http://") || t.startsWith("https://")) && t.includes(".torrent");
}

/** "1.2 MB/s", or "" when idle. */
function formatSpeed(bytesPerSec: number): string {
  if (!bytesPerSec || bytesPerSec <= 0) return "";
  return `${formatBytes(bytesPerSec)}/s`;
}

/** A file-type icon for a Library/torrent file, by mime or extension. */
function torrentFileIcon(f: { mimetype: string | null; name: string }): IconName {
  const mime = (f.mimetype ?? "").toLowerCase();
  const ext = f.name.split(".").pop()?.toLowerCase() ?? "";
  if (mime.startsWith("video/") || ["mkv", "mp4", "avi", "mov", "webm", "m4v"].includes(ext)) {
    return "fileVideo";
  }
  if (mime.startsWith("image/") || ["jpg", "jpeg", "png", "gif", "webp", "bmp"].includes(ext)) {
    return "fileImage";
  }
  if (mime.startsWith("audio/") || ["mp3", "flac", "m4a", "wav", "ogg", "opus"].includes(ext)) {
    return "fileAudio";
  }
  if (["zip", "rar", "7z", "tar", "gz", "xz", "bz2"].includes(ext)) {
    return "fileArchive";
  }
  return "file";
}

/** An inline "working…" marker, shown while a vault blob is decrypted. */
function spinner(): HTMLElement {
  const el = document.createElement("span");
  el.className = "spinner";
  el.setAttribute("aria-hidden", "true");
  return el;
}

/** Human wording for the Tor state. Accurate, not reassuring. */
function torPhaseLabel(phase: TorPhase): string {
  switch (phase) {
    case "ready":
      return "Tor Connected";
    case "starting":
      return "Connecting to Tor";
    case "failed":
      return "Tor Failed";
    case "unavailable":
      return "Tor Unavailable";
    default:
      return "Tor Disconnected";
  }
}

/**
 * The line under the update bar. The plugin reports bytes, not time, so the
 * copy stays with what is actually known: a percentage while the total is
 * known, raw bytes when it is not, and a plain "installing" once the download
 * is done and the signature check is running.
 */
function updateProgressLabel(p: UpdateProgress): string {
  if (p.installing) return "Installing… Hush will restart when it is done.";
  if (p.total !== null && p.percent !== null) {
    return `Downloading… ${Math.round(p.percent)}% of ${formatBytes(p.total)}`;
  }
  if (p.downloaded > 0) return `Downloading… ${formatBytes(p.downloaded)}`;
  return "Starting the download…";
}

/**
 * Why this vault cannot edit a conclave's paper, in the words the surface
 * shows, or `null` when it can.
 *
 * Two reasons, and they are not the same sentence: an ended conclave is
 * read-only on every peer, while a vault that is no longer a joined trustee
 * (kicked, or an invitation never accepted) has the fan-out refused. The daemon
 * refuses both writes, so the editor must not offer one either.
 */
function paperReadOnlyReason(
  conclave: Conclave | undefined,
  me: string,
): string | null {
  if (!conclave) return null;
  if (conclave.ended) {
    return "This conclave has ended, so the paper is read-only.";
  }
  const mine = conclave.trustees.find((t) => t.accountId === me);
  if (mine?.state !== "joined") {
    return "You are no longer a trustee of this conclave, so the paper is read-only.";
  }
  return null;
}

/**
 * The Zines masthead's date line: the day the page is being read, in the
 * reader's own locale.
 *
 * `undefined` rather than a fixed locale on purpose — a date line that reads
 * like a wire dateline to a reader in Nebraska and like a foreign stamp to a
 * reader in Osaka is worse than no dateline at all.
 */
function zinesDateLine(now: Date = new Date()): string {
  return now.toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
  });
}

export class App {
  private root: HTMLElement;
  private status: StatusResponse | null = null;
  private contacts: ContactDto[] = [];
  private rail: Rail = "chats";
  private selectedNick: string | null = null;
  /** The mounted IRC pane, kept so its store subscription is torn down when
   * the rail changes instead of patching a detached DOM. */
  private ircPane: IrcPane | null = null;
  /** Last IRC unread total, so the rail badge repaints only on a change. */
  private lastIrcUnread = -1;
  /** Scroll state of the thread pane from the render that is being replaced.
   * Keyed by the conversation the old DOM was showing: a position only makes
   * sense within one conversation. */
  private chatScrollPos: {
    nick: string | null;
    atBottom: boolean;
    top: number;
  } | null = null;
  /** The conversation the thread pane currently shows, for that keying. */
  private renderedNick: string | null = null;
  /** Where the freshly built thread pane should be scrolled once it is in the
   * document. Decided while the pane is still detached — a detached element
   * has no layout, so setting scrollTop before attachment silently clamps to
   * zero and the reader lands on the first message. */
  private pendingChatScroll:
    | { nick: string; mode: "bottom" }
    | { nick: string; mode: "restore"; top: number }
    | null = null;
  /** Set when the window becomes visible again: the next thread paint opens
   * on the newest message, because returning to the window is a fresh read. */
  private bottomOnNextChatRender = false;
  private pollTimer: number | null = null;
  private unlockedAt = 0;
  /** Relay-switch poller and last-seen state, so the pill repaints only on change. */
  private torPillTimer: number | null = null;
  private torPhase: TorPhase | null = null;
  private torMessage: string | null = null;
  private torMode = false;
  private torApplied = false;
  private torOnionUrl: string | null = null;
  /** Whether the daemon's probe last reached the relay. `null` = not yet
   * known. This, not Tor's phase, decides the shield's colour: Tor stays
   * "ready" long after the network is gone. */
  private torReachable: boolean | null = null;
  /** Guards one in-flight auto-start of the shell's Tor. */
  private torAutoBusy = false;
  /** The last failed action on the Tor surfaces, shown in the settings section. */
  private torActionError: string | null = null;
  private search = "";
  private settingsSection: SettingsSection = "appearance";
  private liveRegion!: HTMLElement;
  private addFriendOpen = false;
  private profileNick: string | null = null;
  private paletteOpen = false;
  /** True while the Vault pane is showing the exchanged-files browser. */
  private filesOpen = false;
  private filesContactNick: string | null = null;
  private filesDirection: "received" | "sent" = "received";
  private recoveryModalCode: string | null = null;
  private mailFolder: MailFolder = "inbox";
  // --- Reads (news reader pane) ---
  /** Which source the list shows: the built-in or a user RSS feed. */
  private readsSource: string = HK_NEWS.id;
  private readsSourceTitle: string = HK_NEWS.title;
  private readsSources: ReadsSourceDto[] = [HK_NEWS];
  /** The typed-but-not-yet-added feed URL; state, so re-renders keep it. */
  private readsSourceDraft = "";
  /** In-flight "Add feed" submit, to disable the dialog and show progress. */
  private readsSourceBusy = false;
  /** Whether the "Add a feed" dialog is open. */
  private readsAddOpen = false;
  /**
   * The add dialog's own error, shown beside the field it belongs to. The pane's
   * `readsError` renders in the middle column, which is exactly why a failed add
   * looked like "nothing happened" — the reason was nowhere near the control.
   */
  private readsAddError: string | null = null;
  private readsPage = 0;
  private readsStories: (ReadsStoryDto & { saved: boolean; read: boolean })[] = [];
  private readsDone = false;
  private readsLoading = false;
  private readsError: string | null = null;
  /** "feed" = a source page; "saved" = Saved. */
  private readsMode: "feed" | "saved" = "feed";
  /** The story open in the reading pane. */
  private readsOpenId: string | null = null;
  private readsDetail: ReadsStoryDetailDto | null = null;
  private readsDetailLoading = false;
  /** Which rendering the main pane shows for the open story. */
  private readsView: "reader" | "comments" = "comments";
  private readsReaderHtml: string | null = null;
  private readsReaderLoading = false;
  private readsReaderError: string | null = null;
  private readsCollapsed = new Set<string>();
  /** Set while the open-original confirm modal is up. */
  private readsConfirmUrl: string | null = null;
  // --- Notes (private sticky-paper board, reached from the Vault pane) ---
  /** True while the Vault pane is showing the board instead of the bento. */
  private notesOpen = false;
  private notesState: NotesStateDto | null = null;
  private notesLoading = false;
  private notesError: string | null = null;
  /** The note open in the editor, or null for the list. */
  private notesEditingId: string | null = null;
  /** True while the board is showing the note editor rather than the list. */
  private notesEditorOpen = false;
  /** Draft fields for the open editor, kept in state so re-renders keep typing. */
  private notesDraftTitle = "";
  private notesDraftBody = "";
  private notesSaving = false;
  /** The note awaiting delete confirmation. */
  private notesDeleteId: string | null = null;
  /** The note shown in the read-only view popup, or null when it is closed. */
  private notesViewingId: string | null = null;
  /** The list search query; filters by title, body and checklist text. */
  private notesSearch = "";
  // --- Torrents (TorBox client pane) ---
  /** Which tab the Torrents pane shows. */
  private torrentsView: "active" | "library" = "active";
  /** The typed-but-not-yet-added magnet/URL; state, so re-renders keep it. */
  private torrentsAddInput = "";
  /** In-flight add, to disable the button and show progress. */
  private torrentsAddBusy = false;
  /** The Add bar's own error, shown beside the field it belongs to. */
  private torrentsAddError: string | null = null;
  /** True while a .torrent file is dragged over the Add bar. */
  private torrentsDragOver = false;
  /** Library folder path (segments of `ItemFile.path`), for breadcrumbs. */
  private torrentsLibraryPath: string[] = [];
  private torrentsLibraryView: "grid" | "list" = "list";
  private torrentsLibrarySearch = "";
  private torrentsLibrarySort: "date" | "name" | "size" = "date";
  /** The Library entry awaiting delete confirmation. */
  private torrentsDeleteId: string | null = null;
  /**
   * Blob id of the Library file currently being decrypted out of the vault for
   * an open or export. Decrypting a multi-GB file is not instant, so the row
   * shows a spinner and its actions are disabled rather than looking inert.
   */
  private torrentsBusyBlob: string | null = null;
  /** Dismisses the "Decrypting…" toast when the busy action settles. */
  private torrentsBusyToast: (() => void) | null = null;
  /** Settings > TorBox field state. */
  private torboxKeyDraft = "";
  private torboxKeyVisible = false;
  private torboxTestOk: string | null = null;
  private torboxTestError: string | null = null;
  private torboxBusy = false;
  /** The conversation open in the reading pane. */
  private mailThreadId: string | null = null;
  /** The message the user clicked, highlighted inside its conversation. */
  private mailOpenId: string | null = null;
  /** The incoming drop open in the reading pane, when the folder is Drops. */
  private dropInboxOpenId: string | null = null;
  /** The account's drop key status, loaded when the Drops folder opens. */
  private dropInboxKey: DropsKeyDto | null = null;
  private mailSearch = "";
  private mailComposeOpen = false;
  /** Set when compose is editing an existing draft rather than a new message. */
  private mailDraftId: string | null = null;
  /** Reply target and thread, when compose was opened from a conversation. */
  private mailReplyTo: string | null = null;
  private mailReplyThread: string | null = null;
  /** Subject to prefill, for a reply. */
  private mailSubjectSeed: string | null = null;
  /**
   * The open compose's fields.
   *
   * Held here rather than read back out of the DOM because compose re-renders
   * (adding an attachment, loading a draft's files) and a rebuilt field would
   * otherwise forget what was typed. The body is HTML from the rich-text
   * editor.
   */
  private mailComposeTo = "";
  private mailComposeSubject = "";
  private mailComposeBody = "";
  /**
   * Attachments held for the open compose.
   *
   * Bytes rather than refs, because both save and send post the bytes, and a
   * draft being edited has to carry its existing files forward.
   */
  private mailComposeFiles: { name: string; dataBase64: string; size: number }[] = [];
  private mailHandle: string | null = null;
  private vaultName = DEFAULT_VAULT_NAME;

  // ------------------------------------------------------------------ drops
  /** Tor status shown on the Drops rail and in the open-Drop panel. */
  private dropsTor: TorStatus = {
    phase: "stopped",
    socks_port: null,
    binary: null,
    message: null,
  };
  private dropsPollTimer: number | null = null;
  /** Whether the backend will allow the development/test Drop on this machine. */
  private dropsDevAllowed = false;
  /** Whether to reveal directory entries whose address is not yet verified. */
  private dropsShowUnverified = false;
  /** The id of the Drop open in its own window, if any. */
  private dropsOpenId: string | null = null;
  /** The Drop whose Open request is in flight, so only that card says so. */
  private dropsOpeningId: string | null = null;
  /** True while an explicit Tor connect is in flight. */
  private dropsTorBusy = false;
  /** The last Drop error, shown inline rather than as a toast. */
  private dropsError: string | null = null;
  /**
   * The contact whose Delete confirmation is open, if any.
   *
   * Deletion lives here and nowhere else: the old remove button in the profile
   * modal was a second, buried way to do the same thing, which is exactly the
   * kind of split the Trusts rail exists to remove.
   */
  private trustDeleteNick: string | null = null;

  /**
   * What the user has typed into the delete dialog's confirmation field.
   *
   * Kept in memory only so a background re-render does not wipe it mid-typing;
   * it is cleared every time the dialog opens, so a previous confirmation can
   * never carry over to the next person.
   */
  private trustDeleteConfirm = "";

  /**
   * The conclave open in the Conclaves rail, if any.
   *
   * Like the mail reading pane, the selection is held outside the DOM so a
   * background re-render cannot lose it.
   */
  private conclaveId: string | null = null;
  /**
   * Which surface of the conclave detail is showing: the chat or the shared
   * Paper. Reset to chat whenever the selection changes.
   */
  private conclaveTab: "chat" | "paper" = "chat";
  /**
   * The stable host element for the Paper editor.
   *
   * render() rebuilds the pane from scratch on every pass, but the
   * ProseMirror view is built once per paper session and re-parented into the
   * fresh pane, so the caret, focus and undo history survive re-renders (the
   * same doctrine as the reader pane's stable DOM).
   */
  private paperHost: HTMLElement | null = null;
  /**
   * The Zines pane's state.
   *
   * Phase 1 is local and offline: this vault authors zines under its pen name
   * and holds whatever else it has been given. Everything the pane needs to
   * paint — the corpus, its sections, the pen name — arrives in one
   * `ZinesViewDto`, because three round trips would be three chances to render
   * a pane that disagrees with itself.
   */
  private zinesView: ZinesViewDto | null = null;
  private zinesLoading = false;
  private zinesError: string | null = null;
  /** The active section filter, or null for the whole corpus. */
  private zinesSection: string | null = null;
  /**
   * The active byline filter, or null for every byline.
   *
   * Applied to the corpus this vault already holds rather than asked of the
   * daemon's `pen` query, because the front page and its strip are both derived
   * from that corpus: a filter that came back as a second, differently-ordered
   * list is a page that can disagree with its own counts.
   *
   * A byline is not an identity — it is free text, and two pen names may share
   * one — so this narrows to "everything signed by this byline", which is what a
   * reader clicking a byline means. The honest copy says byline, not author.
   */
  private zinesPen: string | null = null;
  /**
   * The pane's two view controls.
   *
   * Both are display decisions, not corpus changes: `Mine only` narrows the
   * page to what this vault's pen name wrote, and `Show muted` decides whether
   * muted stories appear at all. They live in the pane because that is what the
   * pane is for — the corpus itself is on the front page.
   */
  private zinesMineOnly = false;
  private zinesShowMuted = true;
  /**
   * Where the front page was scrolled when a story was opened, so coming back
   * lands the reader where they left rather than at the masthead. Captured on
   * the way in, applied after the pane has layout, cleared once used.
   */
  private pendingZinesScroll: number | null = null;
  /** The zine open in the reading pane. */
  private zinesOpenId: string | null = null;
  private zinesDetail: ZineViewDto | null = null;
  private zinesDetailError: string | null = null;
  /**
   * Whether the draft editor is up, and which zine it is revising.
   *
   * `zinesDraftZineId` is null for the pen name's first zine: the daemon takes
   * the byline from the vault and treats an absent id as "my own", which is the
   * only thing Phase 1 can author.
   */
  private zinesEditing = false;
  private zinesDraftZineId: string | null = null;
  /** The draft's title and raw section input, held outside the DOM for the
   *  same reason the conclave form is: a re-render rebuilds the fields. */
  private zinesDraftTitle = "";
  private zinesDraftSections = "";
  /** The body the editor is seeded with when the host is first built. Held
   *  here because `render()` rebuilds the pane around the editor. */
  private zinesDraftSeedHtml = "";
  /**
   * The stable host for the draft editor, built once per draft and re-parented
   * into the fresh pane on every re-render — the same doctrine as `paperHost`.
   */
  private zinesEditorHost: HTMLElement | null = null;
  /**
   * Set by the editor's change callback, so Publish can be enabled in place
   * without re-rendering the pane the author is typing in.
   */
  private zinesPublishBtn: HTMLButtonElement | null = null;
  private zinesPenNameOpen = false;
  private zinesPenNameDraft = "";
  private zinesPenNameError: string | null = null;
  private zinesForgetId: string | null = null;
  /**
   * The share dialog: which post's read key is being handed out, and to whom.
   *
   * The contact is held here rather than read back out of the picker for the
   * same reason the draft's fields are: a re-render rebuilds the control.
   */
  private zinesShareId: string | null = null;
  private zinesShareContact = "";
  private zinesShareBusy = false;
  private zinesShareError: string | null = null;
  /**
   * The comment being written on the open story.
   *
   * Outside the DOM for the same reason: the pane re-renders on a poll, and a
   * half-typed comment must not go with it.
   */
  private zinesCommentDraft = "";
  private zinesCommentBusy = false;
  private zinesCommentError: string | null = null;
  /** Which pending comment is mid-accept or mid-discard, so its row can say so. */
  private zinesCommentBusyHash: string | null = null;
  /**
   * The stable host for the comment box, built once per open story and
   * re-parented into the fresh article on every re-render — the same doctrine
   * as the draft editor's host, because a poll repaints the page under the
   * reader and a half-typed comment must not go with it.
   */
  private zinesCommentHost: HTMLTextAreaElement | null = null;
  private conclaveCreateOpen = false;
  /**
   * The in-progress create-conclave form.
   *
   * Held here rather than read back out of the DOM because the picker re-renders
   * on every selection; a rebuilt field would forget what was typed.
   */
  private conclaveCreateName = "";
  private conclaveCreateTrustees: string[] = [];
  /** The invite dialog's pending selection. */
  private conclaveInviteOpen = false;
  private conclaveInviteTrustees: string[] = [];

  /**
   * The conclave the end dialog is open for, and what the user has typed into
   * its confirmation field.
   *
   * Held here so a background re-render cannot wipe the typing mid-entry, and
   * cleared every time the dialog opens so a previous confirmation can never
   * carry over to the next conclave.
   */
  private conclaveEndId: string | null = null;
  private conclaveEndConfirm = "";

  /**
   * The conclave the delete dialog is open for, and what the user has typed
   * into its confirmation field.
   *
   * Held here for the same reason the end dialog's field is: a background
   * re-render must not wipe the typing mid-entry, and opening always clears it
   * so a previous confirmation can never carry over to the next conclave.
   */
  private conclaveDeleteId: string | null = null;
  private conclaveDeleteConfirm = "";

  // --- Wallet pane ---------------------------------------------------------

  /**
   * The phrase shown once, right after creation, until the user says they have
   * written it down (or leaves the pane). Held here and nowhere else: the
   * daemon returns it exactly once and never again, so losing it loses it.
   */
  private walletPhrase: string | null = null;
  /** Network chosen for create/restore; only `mainnet` is offered in the UI. */
  private walletNetwork = "mainnet";
  /** Which chain the pane is showing. Bitcoin first: it is the default. */
  private walletChain: WalletChain = "bitcoin";
  /** What the user has typed into the restore field. */
  private walletRestoreInput = "";
  /** Send form fields. `walletSendAmount` is in the unit named by the field. */
  private walletSendTo = "";
  private walletSendAmount = "";
  private walletSendFee = String(DEFAULT_FEE_RATE);
  /**
   * The ERC-20 contract to send, or null for the native coin. Held per pane
   * rather than per token, because the field is a single selector.
   */
  private walletSendToken: string | null = null;
  /** What the user has typed into the add-token field. */
  private walletTokenInput = "";
  /** The txid of the last successful send, shown until the pane is left. */
  private walletLastTx: {
    chain: WalletChain;
    txid: string;
    fee_sat: number | null;
  } | null = null;
  /** The last wallet error, shown inline rather than as a toast. */
  private walletError: string | null = null;
  /** What the user has typed to confirm wiping the wallet. */
  private walletWipeConfirm = "";
  /** A wallet operation is in flight; the pane's actions are disabled. */
  private walletBusy = false;
  /** The proxy last reported to the daemon for the wallet, so the status poll
   *  does not repost the same port every tick. */
  private walletProxy: string | null = null;
  /** A wallet-requested Tor start is in flight, so the poll does not stack them. */
  private walletTorBusy = false;
  /** True while the Vault pane is showing the wallet instead of the bento. */
  private walletOpen = false;

  /** The running build's version, for Settings → About. Null until asked for. */
  private appVersion: string | null = null;
  /** The update waiting on the user's choice, from the startup probe or a manual check. */
  private pendingUpdate: AvailableUpdate | null = null;
  /** Download progress while an update installs; null when no install is running. */
  private updateProgress: UpdateProgress | null = null;
  /** A manual check is in flight, so its button stays disabled. */
  private updateCheckBusy = false;
  /**
   * The updater's own line in Settings → About: the outcome of the last manual
   * check. The startup probe writes nothing here — it is silent by design.
   */
  private updateNote: { text: string; error: boolean } | null = null;
  /**
   * The open prompt's progress nodes, patched in place as chunks arrive.
   * A full `render()` per chunk would rebuild the whole app several times a
   * second; holding these two nodes keeps the download smooth.
   */
  private updateProgressEl: {
    fill: HTMLElement;
    label: HTMLElement;
  } | null = null;

  /** Read/star/archive/trash, straight from `ui-prefs.json`. */
  private mailPrefs(): MailFlags {
    return mailFlags();
  }

  constructor(root: HTMLElement) {
    this.root = root;
  }

  async start(): Promise<void> {
    // The shell injects the transport before the page runs. A plain browser
    // never sets it, so waiting there only stalls every dev reload.
    if (isTauriShell()) {
      for (let i = 0; i < 30; i++) {
        if (window.__HUSH_API__) break;
        await sleep(50);
      }
    }
    this.liveRegion = document.createElement("div");
    this.liveRegion.setAttribute("aria-live", "polite");
    this.liveRegion.className = "sr-only";
    document.body.appendChild(this.liveRegion);
    // Torrents: the daemon pipeline runs on its own; the store polls and, when
    // an item finishes or fails, says so even if the pane is not on screen.
    setTorrentsHooks({
      finished: (item) => {
        showToast(`Torrent ready: ${item.name}`, { kind: "success" });
        this.announce(`Torrent ready: ${item.name}`);
      },
      failed: (item) => {
        showToast(`Torrent failed: ${item.name} — ${item.message ?? "unknown error"}`, {
          kind: "error",
        });
        this.announce(`Torrent failed: ${item.name}`);
      },
    });
    subscribeTorrents(() => {
      if (this.rail !== "torrents") return;
      // Never clobber the Add field while the user is typing in it.
      const ae = document.activeElement;
      if (ae instanceof HTMLInputElement && ae.closest(".torrents-pane")) return;
      this.render();
    });
    subscribeWallet(() => {
      if (this.rail !== "vault" || !this.walletOpen) return;
      // While an operation is in flight its own handler owns the re-render;
      // this keeps one action from painting twice.
      if (this.walletBusy) return;
      // Never clobber a field the user is typing in, and never wipe the
      // one-time phrase card out from under them mid-copy.
      const ae = document.activeElement;
      if (ae instanceof HTMLInputElement && ae.closest(".wallet-pane")) return;
      if (ae instanceof HTMLTextAreaElement && ae.closest(".wallet-pane")) return;
      if (this.walletPhrase) return;
      this.render();
    });
    // IRC: the pane patches its own DOM, so this only keeps the rail badge
    // honest. While the rail is up the badge is patched in place; otherwise a
    // repaint refreshes it along with everything else.
    subscribeIrc(() => {
      const total = ircUnreadTotal();
      const mentions = ircMentionCount();
      const key = total + mentions * 100000;
      if (key === this.lastIrcUnread) return;
      this.lastIrcUnread = key;
      if (this.rail === "irc") {
        this.updateIrcRailBadge(total, mentions);
      } else {
        this.render();
      }
    });
    document.addEventListener("keydown", (e) => {
      // IRC keeps its own shortcuts while its rail is up: Ctrl+Tab cycles
      // windows, Ctrl+W closes the active one.
      if (this.rail === "irc" && (e.ctrlKey || e.metaKey) && e.key === "Tab") {
        e.preventDefault();
        this.ircPane?.cycle(e.shiftKey ? -1 : 1);
        return;
      }
      if (this.rail === "irc" && (e.ctrlKey || e.metaKey) && (e.key === "w" || e.key === "W")) {
        e.preventDefault();
        this.ircPane?.closeActive();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "k") {
        e.preventDefault();
        this.paletteOpen = !this.paletteOpen;
        this.render();
      }
      if (e.key === "Escape") {
        // The update prompt is the topmost layer when it is up, so Escape
        // belongs to it first. A download in progress is not cancellable, so
        // it keeps the dialog rather than closing it over a running install.
        if (this.pendingUpdate && !this.updateProgress) {
          void this.dismissUpdatePrompt();
          return;
        }
        this.paletteOpen = false;
        this.addFriendOpen = false;
        this.readsAddOpen = false;
        this.readsAddError = null;
        this.profileNick = null;
        this.closeTrustDelete();
        this.conclaveCreateOpen = false;
        this.conclaveInviteOpen = false;
        this.closeConclaveEnd();
        this.closeConclaveDelete();
        this.torrentsDeleteId = null;
        // The IRC pane owns its own dialogs and menus; a repaint would drop
        // the caret out of its input for no reason.
        if (this.rail !== "irc") this.render();
      }
    });
    // IRC runs its own sockets; wiring it up here (rather than on first visit
    // to the rail) means unread counts and live connections survive a switch.
    await initIrcStore();
    await this.refreshStatus();
    this.render();
    this.startUpdateProbe();
  }

  /**
   * The launch-time half of the updater: ask for the running version (About
   * shows it), then wait out a short delay and probe once for a newer release.
   *
   * Both are deliberately off the critical path. The probe is silent: offline,
   * a 404, or an unconfigured public key all end with nothing on screen,
   * because the user never asked for this check. Only a real update surfaces,
   * and even then it waits its turn — `render` shows the prompt whenever the
   * next paint happens, which keeps it off the unlock screen.
   */
  private startUpdateProbe(): void {
    if (!isTauriShell()) return;
    void getCurrentVersion().then((version) => {
      if (!version) return;
      this.appVersion = version;
      if (this.rail === "settings" && this.settingsSection === "about") {
        this.render();
      }
    });
    window.setTimeout(() => {
      void checkForUpdates({ silent: true }).then((outcome) => {
        if (outcome.kind !== "available") return;
        this.pendingUpdate = outcome.update;
        this.render();
      });
    }, STARTUP_UPDATE_DELAY_MS);
  }

  /** A manual "Check for updates": the outcome is always shown, either way. */
  private async runManualUpdateCheck(): Promise<void> {
    if (this.updateCheckBusy) return;
    this.updateCheckBusy = true;
    this.updateNote = null;
    this.render();
    const outcome = await checkForUpdates();
    this.updateCheckBusy = false;
    if (outcome.kind === "available") {
      this.pendingUpdate = outcome.update;
      this.updateNote = null;
    } else if (outcome.kind === "up-to-date") {
      this.updateNote = { text: "You're on the latest version.", error: false };
    } else {
      this.updateNote = {
        text: outcome.kind === "failed" ? outcome.message : outcome.reason,
        error: true,
      };
    }
    this.render();
  }

  /**
   * Start the download the prompt announced. The prompt stays up and becomes
   * the progress view, so the dialog never jumps out from under the cursor.
   *
   * A failure here — offline, a bad endpoint, a signature that does not match
   * the configured key — leaves the running app untouched: the plugin verifies
   * the artifact before it replaces anything, so there is no half-installed
   * state to recover from. The message goes to the About line and a toast, and
   * the prompt closes.
   */
  private async beginUpdateInstall(): Promise<void> {
    this.updateProgress = {
      downloaded: 0,
      total: null,
      percent: null,
      installing: false,
    };
    this.render();
    try {
      await downloadAndInstallUpdate((p) => {
        this.updateProgress = p;
        this.patchUpdateProgress(p);
      });
      // `relaunch` replaces this process, so this line is only reached on the
      // way out. Nothing to clean up.
    } catch (e) {
      const message = e instanceof Error ? e.message : "The update failed.";
      this.closeUpdatePrompt();
      this.updateNote = { text: message, error: true };
      this.render();
      showToast(message, { kind: "error" });
      await discardPendingUpdate();
    }
  }

  /** Close the prompt and let go of the backend's update handle. */
  private async dismissUpdatePrompt(): Promise<void> {
    this.closeUpdatePrompt();
    this.render();
    await discardPendingUpdate();
  }

  private closeUpdatePrompt(): void {
    this.pendingUpdate = null;
    this.updateProgress = null;
    this.updateProgressEl = null;
  }

  /**
   * Move the bar and its label without a repaint. `downloadAndInstall` fires
   * once per chunk — several times a second on a fast link — and rebuilding the
   * whole app that often would make the download stutter.
   */
  private patchUpdateProgress(p: UpdateProgress): void {
    const el = this.updateProgressEl;
    if (!el) return;
    if (p.percent === null) {
      el.fill.classList.add("is-indeterminate");
      el.fill.style.width = "";
    } else {
      el.fill.classList.remove("is-indeterminate");
      el.fill.style.width = `${p.percent}%`;
    }
    el.label.textContent = updateProgressLabel(p);
  }

  /**
   * The update prompt. Two states in one dialog: what is available (with the
   * two ways out), then the download and install in place of those buttons.
   */
  private renderUpdatePrompt(): HTMLElement {
    const update = this.pendingUpdate!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Software update");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = `Hush ${update.version} is available`;
    const sub = document.createElement("p");
    sub.className = "muted text-13";
    sub.style.marginTop = "var(--space-2)";
    sub.textContent = `You are running ${update.currentVersion}.`;
    modal.append(h, sub);

    if (update.notes) {
      const notes = document.createElement("div");
      notes.className = "update-notes text-13";
      notes.textContent = update.notes;
      modal.appendChild(notes);
    }

    if (this.updateProgress) {
      const track = document.createElement("div");
      track.className = "update-bar";
      const fill = document.createElement("div");
      fill.className = "update-bar-fill";
      const pct = this.updateProgress.percent;
      if (pct === null) {
        // No content length from the server: an indeterminate sweep rather
        // than a bar pinned at zero, which reads as a stall.
        fill.classList.add("is-indeterminate");
      } else {
        fill.style.width = `${pct}%`;
      }
      track.appendChild(fill);
      const label = document.createElement("p");
      label.className = "muted text-13";
      label.style.marginTop = "var(--space-2)";
      label.textContent = updateProgressLabel(this.updateProgress);
      // Held for `patchUpdateProgress`, which updates these in place.
      this.updateProgressEl = { fill, label };
      modal.append(track, label);
    } else {
      const row = document.createElement("div");
      row.className = "modal-actions";
      const later = document.createElement("button");
      later.type = "button";
      later.className = "btn-secondary pressable";
      later.textContent = "Later";
      later.onclick = () => void this.dismissUpdatePrompt();
      const now = document.createElement("button");
      now.type = "button";
      now.className = "btn-primary pressable";
      now.textContent = "Update now";
      now.onclick = () => void this.beginUpdateInstall();
      row.append(later, now);
      modal.appendChild(row);
    }

    back.appendChild(modal);
    return back;
  }

  /**
   * Settings → About: the manual half of the updater. The startup probe is
   * invisible by design, so this is the only place a user can ask — and the
   * only place that can answer "you're on the latest version".
   */
  private renderUpdateControls(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "update-controls";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn-secondary pressable";
    btn.textContent = this.updateCheckBusy ? "Checking…" : "Check for updates";
    btn.disabled = this.updateCheckBusy || this.updateProgress !== null;
    btn.onclick = () => void this.runManualUpdateCheck();
    wrap.appendChild(btn);
    if (this.updateNote) {
      const note = document.createElement("p");
      note.className = "text-13";
      note.style.marginTop = "var(--space-2)";
      if (this.updateNote.error) note.style.color = "var(--danger)";
      note.textContent = this.updateNote.text;
      wrap.appendChild(note);
    }
    return wrap;
  }

  private announce(msg: string): void {
    this.liveRegion.textContent = msg;
  }

  private async refreshStatus(): Promise<void> {
    // Only the status call itself can mean "locked". A failure to load an
    // auxiliary resource (contacts, mailbox, conclaves) is a partial refresh,
    // not a failed login, and must not flip the app to the unlock screen.
    let status: StatusResponse | null = null;
    try {
      status = await api.status();
    } catch {
      status = null;
    }
    this.status = status;
    if (!status || status.locked) {
      this.stopPoll();
      return;
    }
    try {
      this.contacts = (await api.contacts()) ?? [];
      setKnownContacts(this.contacts.map((c) => c.nickname));
      if (!this.selectedNick && this.contacts[0]) {
        this.selectedNick = this.contacts[0].nickname;
      }
      // The unlock screen is not the only door into a running vault: a page
      // reload (or a dev hot reload) lands here with the daemon already
      // unlocked, and the thread must have its history just the same.
      const history = await api.messages();
      if (history) loadHistory(history);
      this.mailHandle = status.mail_handle ?? null;
      this.vaultName = status.vault_name ?? DEFAULT_VAULT_NAME;
      await this.reloadMailbox();
      await this.reloadConclaves();
    } catch {
      this.announce(
        "Some data could not be loaded. It will be retried on the next sync.",
      );
    }
    this.startPoll();
  }

  private startPoll(): void {
    if (this.pollTimer) return;
    this.pollTimer = window.setInterval(() => this.pollRecv(), RECV_POLL_MS);
    // Coming back to the window is a fresh read: the conversation on screen
    // opens on its newest message instead of where the window happened to be
    // left, so arriving messages are where the reader looks for them.
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState !== "visible") return;
      if (this.rail === "chats" && this.selectedNick) {
        this.bottomOnNextChatRender = true;
        this.render();
      }
    });
  }

  /** Pull the whole mailbox: messages and drafts arrive together so the two
   *  halves of the Mail rail can never disagree. Returns whether it changed. */
  private async reloadMailbox(): Promise<boolean> {
    const box = await api.mail();
    if (!box) return false;
    return loadMailbox(box.mail ?? [], box.drafts ?? [], box.drops ?? []);
  }

  /** Re-read the conclave list. Returns whether it changed, so the poll can
   *  skip a repaint that would steal the caret out of the conclave composer. */
  private async reloadConclaves(): Promise<boolean> {
    const list = await api.conclaves();
    const invites = await api.pendingInvites();
    if (!list) return false;
    const changed = loadConclaves(list);
    const invitesChanged = invites ? loadPendingInvites(invites) : false;
    return changed || invitesChanged;
  }

  private stopPoll(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
  }

  private async pollRecv(): Promise<void> {
    if (!this.status || this.status.locked) return;
    try {
      // Appear-offline: skip the heartbeat and let the relay's timestamp go
      // stale, so contacts report offline. The daemon's heartbeat loop is
      // suppressed the same way.
      if (!uiPrefsSnapshot().appear_offline) await api.presenceHeartbeat();
      const r = await api.recv();
      const fresh = (await api.contacts()) ?? [];
      const prevOnline = new Map(
        this.contacts.map((c) => [c.nickname, c.online]),
      );
      const presenceChanged =
        fresh.length !== this.contacts.length ||
        fresh.some((c) => prevOnline.get(c.nickname) !== c.online);
      this.contacts = fresh;
      setKnownContacts(fresh.map((c) => c.nickname));
      if (!r) {
        if (presenceChanged) this.render();
        return;
      }
      for (const w of r.warnings) this.announce(w);
      let mailArrived = false;
      let conclaveArrived = false;
      for (const m of r.messages) {
        if (m.kind === "mail") {
          // Reloaded from the daemon rather than appended here, so the message
          // carries the record id the read/unread flags are keyed on.
          mailArrived = true;
          this.announce(`New mail from ${m.from}`);
        } else if (m.kind === "conclave") {
          // The daemon already filed it into the conclave (materialising the
          // conclave first if it was unknown), so re-read rather than guess at
          // the record id here.
          conclaveArrived = true;
          this.announce(`New conclave message from ${m.from}`);
        } else {
          ingestIncoming(m.from, m);
          this.announce(`New message from ${m.from}`);
          // A chat that is on screen is read as it arrives: no unread badge,
          // no sound. Otherwise the sound plays unless the user turned it off
          // globally or for this contact. Mail is silent: its badge is enough.
          if (this.rail === "chats" && this.selectedNick === m.from) {
            markRead(m.from);
          } else if (chatSoundEnabled() && !isChatSoundMuted(m.from)) {
            playChatSound();
          }
        }
      }
      // Mail also changes without an incoming message reaching this poll: the
      // daemon's Outbox flush moves a message from Queued to Sent, and another
      // device can file or delete one. While the Mail rail is up, re-read the
      // mailbox so the list is never stale.
      let mailboxChanged = false;
      if (mailArrived || this.rail === "mail") {
        mailboxChanged = await this.reloadMailbox();
      }
      let conclavesChanged = false;
      if (conclaveArrived || this.rail === "conclaves") {
        conclavesChanged = await this.reloadConclaves();
      }
      // Never repaint over a half-written message, or over a modal that owns
      // text the user is part-way through: a repaint rebuilds the modal from
      // stored state and resets the fields being typed into. The Paper editor
      // counts too: while its surface holds the caret, background traffic
      // (presence flips, other chats) must not detach it and steal focus.
      const editing =
        this.mailComposeOpen ||
        this.conclaveCreateOpen ||
        this.conclaveInviteOpen ||
        this.addFriendOpen ||
        this.readsAddOpen ||
        this.profileNick !== null ||
        this.trustDeleteNick !== null ||
        (this.conclaveTab === "paper" && paperHasFocus()) ||
        (this.rail === "zines" && (this.zinesEditing || zineEditorHasFocus()));
      if (
        (r.messages.length || presenceChanged || mailboxChanged || conclavesChanged) &&
        !editing
      ) {
        this.render();
      }
    } catch {
      /* daemon busy */
    }
  }

  private async lock(): Promise<void> {
    // Flush the paper while the vault is still open. Every paper route answers
    // 401 once it is locked, and the editor's doc dies with the session below,
    // so an edit still inside its flush window would otherwise be lost.
    flushPaperEdits();
    await api.lock();
    clearChatStore();
    clearMailStore();
    clearConclaveStore();
    // The paper's editor doc and its sync loop die with the session; the
    // merged state survives only inside the vault's encrypted sidecar.
    closePaper();
    this.paperHost = null;
    this.conclaveTab = "chat";
    // The draft editor's doc is the only copy of an unpublished revision, and
    // every zines route answers 401 once the vault is locked. Phase 1 has no
    // local persistence for a draft, so it is discarded rather than left on
    // screen looking publishable.
    this.zinesDiscardDraft();
    this.zinesView = null;
    this.zinesDetail = null;
    this.zinesOpenId = null;
    this.zinesSection = null;
    this.zinesPen = null;
    this.zinesMineOnly = false;
    this.zinesShowMuted = true;
    this.pendingZinesScroll = null;
    this.zinesError = null;
    this.zinesDetailError = null;
    this.zinesPenNameOpen = false;
    this.zinesForgetId = null;
    this.zinesShareId = null;
    this.zinesShareContact = "";
    this.zinesShareError = null;
    this.zinesCommentDraft = "";
    this.zinesCommentError = null;
    clearReadsStore();
    clearNotesStore();
    clearTorrentsStore();
    clearWalletStore();
    this.walletOpen = false;
    this.walletPhrase = null;
    this.walletLastTx = null;
    this.walletError = null;
    this.walletSendTo = "";
    this.walletSendAmount = "";
    this.walletRestoreInput = "";
    this.walletWipeConfirm = "";
    clearMockLocal();
    this.readsSource = HK_NEWS.id;
    this.readsSourceTitle = HK_NEWS.title;
    this.readsSources = [HK_NEWS];
    this.readsMode = "feed";
    this.readsOpenId = null;
    this.readsDetail = null;
    this.readsReaderHtml = null;
    this.readsConfirmUrl = null;
    this.notesOpen = false;
    this.notesEditorOpen = false;
    this.notesEditingId = null;
    this.notesState = null;
    this.notesLoading = false;
    this.notesError = null;
    this.notesDraftTitle = "";
    this.notesDraftBody = "";
    this.notesDeleteId = null;
    this.notesViewingId = null;
    this.notesSearch = "";
    this.selectedNick = null;
    this.filesOpen = false;
    this.filesContactNick = null;
    this.mailThreadId = null;
    this.mailOpenId = null;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.mailReplyTo = null;
    this.mailReplyThread = null;
    this.mailSearch = "";
    this.conclaveId = null;
    this.conclaveCreateOpen = false;
    this.conclaveCreateName = "";
    this.conclaveCreateTrustees = [];
    this.conclaveInviteOpen = false;
    this.conclaveInviteTrustees = [];
    this.conclaveEndId = null;
    this.conclaveEndConfirm = "";
    this.conclaveDeleteId = null;
    this.conclaveDeleteConfirm = "";
    this.unlockedAt = 0;
    this.stopPoll();
    await this.refreshStatus();
    this.render();
  }

  private render(): void {
    // The thread pane is rebuilt from scratch on every render, so the reader's
    // place in it has to be carried across. Captured before the wipe: whether
    // the thread was scrolled to the bottom (pinned to new messages) or the
    // user had moved away to read history (leave them there).
    const prevMsgs = this.root.querySelector<HTMLElement>(".chat-messages");
    this.chatScrollPos = prevMsgs
      ? {
          nick: this.renderedNick,
          atBottom:
            prevMsgs.scrollHeight - prevMsgs.scrollTop - prevMsgs.clientHeight < 48,
          top: prevMsgs.scrollTop,
        }
      : null;
    // The IRC pane holds a live store subscription; tear it down before its
    // DOM is discarded, or it would keep patching a detached tree.
    this.ircPane?.unmount();
    this.ircPane = null;
    this.root.innerHTML = "";
    if (!this.status || this.status.locked) {
      this.stopDropsPoll();
      this.stopTorPoll();
      setTorrentsPaneOpen(false);
      this.root.appendChild(this.renderUnlock());
      return;
    }
    // An open conversation is a conversation being read: whatever unread
    // count it carried goes away the moment its pane paints, however the pane
    // was reached — a click, the rail button re-opening the last chat, or an
    // arrival while the chat was already up.
    if (this.rail === "chats" && this.selectedNick) {
      markRead(this.selectedNick);
    }
    const shell = document.createElement("div");
    shell.className = "shell";
    shell.appendChild(this.renderRail());
    if (this.rail === "chats") {
      shell.appendChild(this.renderConvList());
      shell.appendChild(this.renderMainChat());
    } else if (this.rail === "trusts") {
      shell.appendChild(this.renderTrusts());
    } else if (this.rail === "conclaves") {
      shell.appendChild(this.renderConclaves());
    } else if (this.rail === "zines") {
      shell.appendChild(this.renderZinesPane());
      shell.appendChild(this.renderZinesPage());
    } else if (this.rail === "reads") {
      shell.appendChild(this.renderReadsNav());
      shell.appendChild(this.renderReadsList());
      shell.appendChild(this.renderReadsMain());
    } else if (this.rail === "torrents") {
      shell.appendChild(this.renderTorrents());
    } else if (this.rail === "irc") {
      shell.appendChild(this.renderIrc());
    } else if (this.rail === "mail") {
      shell.appendChild(this.renderMailFolders());
      shell.appendChild(this.renderMailMain());
    } else if (this.rail === "drops") {
      shell.appendChild(this.renderDrops());
    } else if (this.rail === "vault") {
      shell.appendChild(
        this.walletOpen
          ? this.renderWallet()
          : this.notesOpen
            ? this.renderNotes()
            : this.filesOpen
              ? this.renderFiles()
              : this.renderVault(),
      );
    } else {
      shell.appendChild(this.renderSettings());
    }
    this.root.appendChild(shell);
    this.startTorPoll();
    setTorrentsPaneOpen(this.rail === "torrents");
    setWalletPaneOpen(this.rail === "vault" && this.walletOpen);
    this.renderedNick = this.rail === "chats" ? this.selectedNick : null;
    // Now that the thread pane is in the document and has layout, put the
    // reader where the conversation should open: the newest message, or their
    // place in the history of this same conversation.
    const liveMsgs = this.root.querySelector<HTMLElement>(".chat-messages");
    const pending = this.pendingChatScroll;
    this.pendingChatScroll = null;
    if (liveMsgs && pending && pending.nick === this.renderedNick) {
      if (pending.mode === "bottom") {
        liveMsgs.scrollTop = liveMsgs.scrollHeight;
      } else {
        liveMsgs.scrollTop = Math.min(pending.top, liveMsgs.scrollHeight);
      }
    }
    // The front page scrolls like any long page, so a story opened from halfway
    // down it puts the reader back at that point on the way out — the same
    // carry-across as the thread pane above, and for the same reason. Only the
    // front page consumes it: a story is also a `.zines-page`, and consuming
    // the position there would throw it away before the reader came back.
    const liveZines = this.root.querySelector<HTMLElement>(".zines-page");
    if (
      liveZines &&
      this.pendingZinesScroll !== null &&
      liveZines.querySelector(".zines-masthead")
    ) {
      liveZines.scrollTop = Math.min(this.pendingZinesScroll, liveZines.scrollHeight);
      this.pendingZinesScroll = null;
    }
    if (this.paletteOpen) this.root.appendChild(this.renderPalette());
    if (this.addFriendOpen) this.root.appendChild(this.renderAddFriend());
    if (this.readsAddOpen) this.root.appendChild(this.renderReadsAddModal());
    if (this.profileNick) this.root.appendChild(this.renderProfile());
    if (this.recoveryModalCode) this.root.appendChild(this.renderRecoveryModal());
    if (this.trustDeleteNick) this.root.appendChild(this.renderTrustDeleteModal());
    if (this.conclaveCreateOpen) this.root.appendChild(this.renderConclaveCreateModal());
    if (this.conclaveInviteOpen) this.root.appendChild(this.renderConclaveInviteModal());
    if (this.conclaveEndId) this.root.appendChild(this.renderConclaveEndModal());
    if (this.conclaveDeleteId) this.root.appendChild(this.renderConclaveDeleteModal());
    if (this.readsConfirmUrl) this.root.appendChild(this.renderReadsConfirm());
    if (this.torrentsDeleteId) this.root.appendChild(this.renderTorrentsDeleteModal());
    if (this.notesViewingId) this.root.appendChild(this.renderNoteViewModal());
    if (this.zinesPenNameOpen) this.root.appendChild(this.renderZinesPenNameModal());
    if (this.zinesForgetId) this.root.appendChild(this.renderZinesForgetModal());
    if (this.zinesShareId) this.root.appendChild(this.renderZinesShareModal());
    if (this.pendingUpdate) this.root.appendChild(this.renderUpdatePrompt());
  }

  private async maybeShowRecoveryCode(): Promise<void> {
    try {
      const pending = await api.recoveryPending();
      if (pending?.code) this.recoveryModalCode = pending.code;
    } catch {
      /* no pending code */
    }
  }

  private renderRecoveryModal(): HTMLElement {
    const code = this.recoveryModalCode!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", "Recovery code");
    modal.innerHTML = `<h2 class="text-22">Save your recovery code</h2><p class="muted text-13">Store this somewhere safe. It can unlock your vault if you forget your passphrase. We will only show it once.</p>`;
    const pre = document.createElement("p");
    pre.className = "text-15";
    pre.style.fontFamily = "monospace";
    pre.style.wordBreak = "break-all";
    pre.textContent = code;
    const btn = document.createElement("button");
    btn.className = "btn-primary pressable";
    btn.style.marginTop = "var(--space-4)";
    btn.textContent = "I've saved it";
    btn.onclick = async () => {
      await api.recoveryAcknowledge();
      this.recoveryModalCode = null;
      await this.refreshStatus();
      this.render();
    };
    modal.append(pre, btn);
    back.appendChild(modal);
    return back;
  }

  private renderUnlock(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "unlock-screen";
    const card = document.createElement("div");
    card.className = "unlock-card";
    const logoWrap = document.createElement("div");
    logoWrap.className = "logo-wrap";
    const ring = document.createElement("div");
    ring.className = "logo-ring breathe";
    const mark = document.createElement("div");
    mark.className = "logo-mark";
    mark.appendChild(icon("vault"));
    logoWrap.append(ring, mark);
    const title = document.createElement("h1");
    title.className = "text-28";
    title.textContent = "Welcome back";
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.textContent = "Your vault stays on this device.";
    const pass = document.createElement("input");
    pass.type = "password";
    pass.className = "input-field";
    pass.placeholder = "Passphrase";
    pass.autocomplete = "current-password";
    pass.style.marginTop = "var(--space-4)";
    const showRow = document.createElement("label");
    showRow.className = "text-13 muted";
    showRow.style.display = "flex";
    showRow.style.gap = "8px";
    showRow.style.marginTop = "8px";
    const showCb = document.createElement("input");
    showCb.type = "checkbox";
    showCb.onchange = () => {
      pass.type = showCb.checked ? "text" : "password";
    };
    showRow.append(showCb, document.createTextNode("Show passphrase"));

    // The pre-login Tor switch. The mode lives in the daemon's prefs (readable
    // while locked), so the choice is remembered before the vault opens and
    // unlock applies it to traffic from the first request on. The onion
    // address is not asked for here: it is once-per-vault setup that lives in
    // the relay settings.
    const torRow = document.createElement("div");
    torRow.style.display = "flex";
    torRow.style.gap = "10px";
    torRow.style.alignItems = "center";
    torRow.style.marginTop = "10px";
    const torSw = this.makeSwitch(false);
    const torText = document.createElement("span");
    torText.className = "text-13";
    torText.textContent = "Use Tor for extra security";
    torText.title = "The relay is reached over the Tor network, so it never learns your IP.";
    torRow.append(torSw.el, torText);

    void api
      .relayConfig()
      .then((cfg) => {
        if (cfg) torSw.input.checked = cfg.tor_mode;
      })
      .catch(() => {
        /* the daemon may not be up yet; the switch stays off. */
      });

    torSw.input.onchange = async () => {
      const want = torSw.input.checked;
      err.hidden = true;
      torSw.input.disabled = true;
      try {
        await this.toggleTorRelay(want);
      } catch (e) {
        torSw.input.checked = !want;
        err.hidden = false;
        err.textContent =
          e instanceof AppError ? e.message : "The switch could not be applied.";
      }
      torSw.input.disabled = false;
    };

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.hidden = true;
    const btn = document.createElement("button");
    btn.className = "btn-primary pressable";
    btn.style.marginTop = "var(--space-4)";
    btn.textContent = "Unlock";
    btn.onclick = async () => {
      err.hidden = true;
      try {
        await api.unlock(pass.value);
        pass.value = "";
        this.unlockedAt = Date.now();
        ring.classList.add("bloom");
        await sleep(400);
        await this.refreshStatus();
        await refreshContactPrefs();
        const history = await api.messages();
        if (history) loadHistory(history);
        await this.maybeShowRecoveryCode();
        this.render();
      } catch (e) {
        card.classList.add("shake");
        setTimeout(() => card.classList.remove("shake"), 300);
        err.hidden = false;
        err.textContent =
          e instanceof AppError ? e.message : "That passphrase didn't work. Try again.";
      }
    };
    const daemon = document.createElement("p");
    daemon.className = "faint text-12";
    daemon.style.marginTop = "var(--space-4)";
    daemon.textContent = getApiBase();
    card.append(logoWrap, title, hint, pass, showRow, torRow, err, btn, daemon);
    wrap.appendChild(card);
    return wrap;
  }

  /**
   * A left-rail button. `badge` is either a plain unread dot (`true`) or, when
   * a number, a pill carrying the unread count — chats and mail show how many
   * messages wait, the other rails stay quiet.
   */
  private railBtn(id: Rail, label: string, ic: IconName, badge: boolean | number = false): HTMLElement {
    const b = document.createElement("button");
    b.className = `rail-btn pressable${this.rail === id ? " active" : ""}`;
    b.dataset.rail = id;
    b.appendChild(icon(ic));
    const span = document.createElement("span");
    span.textContent = label;
    b.appendChild(span);
    if (typeof badge === "number" && badge > 0) {
      const pill = document.createElement("span");
      pill.className = "rail-count";
      pill.textContent = badge > 99 ? "99+" : String(badge);
      pill.setAttribute("aria-label", `${badge} unread`);
      b.appendChild(pill);
    } else if (badge) {
      const dot = document.createElement("span");
      dot.className = "rail-badge";
      dot.setAttribute("aria-label", "Unread");
      b.appendChild(dot);
    }
    b.onclick = () => {
      const wasConclaves = this.rail === "conclaves";
      this.rail = id;
      if (wasConclaves && id !== "conclaves") {
        // Leaving the rail closes the paper: one session at a time, and the
        // pending edits flush before the timers stop.
        this.leavePaperTab();
      }
      this.render();
      // The Mail rail reads the encrypted log, so pick up anything that landed
      // while another rail was showing.
      if (id === "mail") {
        void this.reloadMailbox().then((changed) => {
          if (changed && this.rail === "mail" && !this.mailComposeOpen) this.render();
        });
      }
      if (id === "conclaves") {
        void this.reloadConclaves().then((changed) => {
          if (changed && this.rail === "conclaves" && !this.conclaveCreateOpen) this.render();
        });
      }
      if (id === "zines") {
        // Leaving the rail closes the draft: the editor's doc is the only copy
        // of an unpublished revision, and keeping it mounted would let a
        // cancelled draft reappear on the next visit.
        void this.zinesLoad();
      } else if (this.zinesEditing) {
        this.zinesDiscardDraft();
      }
      if (id === "reads") {
        // Refresh the source list every time the pane opens (user feeds may
        // have changed elsewhere / while locked only the built-in shows).
        void getReadsSources().then((sources) => {
          this.readsSources = sources;
          if (this.rail === "reads") this.render();
        });
        if (!this.readsStories.length && !this.readsLoading) void this.readsLoad();
      }
      if (id === "torrents") {
        // The pipeline may have advanced while another rail was up.
        void refreshTorrents().then(() => {
          if (this.rail === "torrents") this.render();
        });
      }
      if (id === "vault") {
        // Keep the tiles' counts fresh without a request per render.
        void this.notesEnsureLoaded();
        void refreshWallet()
          .then(() => {
            if (this.rail === "vault" && !this.walletOpen) this.render();
          })
          .catch(() => {});
      }
      if (id === "drops") {
        // Tor state lives in the shell, so poll it while the rail is up.
        this.startDropsPoll();
        void this.refreshDrops();
      } else {
        this.stopDropsPoll();
      }
    };
    return b;
  }

  private renderRail(): HTMLElement {
    const rail = document.createElement("nav");
    rail.className = "rail";
    rail.setAttribute("aria-label", "Main");
    // The shield is the app's mark and the connection light in one: filled
    // green when the relay is reachable (clearnet or Tor), orange while Tor
    // is coming up, red when it is not reachable. The words live in its
    // accessible name. Clicking it opens the relay switch in Settings.
    const shield = document.createElement("button");
    shield.type = "button";
    shield.className = "relay-shield pressable";
    shield.onclick = () => {
      this.rail = "settings";
      this.settingsSection = "relay";
      this.render();
    };
    shield.appendChild(icon("vault"));
    this.updateTorPillInto(shield);
    rail.appendChild(shield);
    rail.appendChild(this.railBtn("chats", "Chats", "chats", unreadTotal()));
    rail.appendChild(this.railBtn("trusts", "Trusts", "trusts"));
    rail.appendChild(this.railBtn("conclaves", "Conclaves", "conclaves"));
    rail.appendChild(this.railBtn("zines", "Zines", "zines"));
    rail.appendChild(this.railBtn("reads", "Reads", "reads"));
    rail.appendChild(
      this.railBtn(
        "torrents",
        "Torrents",
        "torrents",
        activeTorrentCount(getTorrentsState()),
      ),
    );
    rail.appendChild(this.railBtn("irc", "IRC", "irc", ircUnreadTotal()));
    rail.appendChild(this.railBtn("mail", "Mail", "mail", unreadCount(this.mailPrefs())));
    rail.appendChild(this.railBtn("drops", "Drops", "lock"));
    const spacer = document.createElement("div");
    spacer.style.height = "40px";
    rail.appendChild(spacer);
    const flex = document.createElement("div");
    flex.className = "rail-spacer";
    rail.appendChild(flex);
    rail.appendChild(this.railBtn("vault", "Vault", "vault"));
    rail.appendChild(this.railBtn("settings", "Settings", "settings"));
    return rail;
  }

  private renderConvList(): HTMLElement {
    const col = document.createElement("div");
    col.className = "conv-list";
    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Chats";
    const plus = document.createElement("button");
    plus.className = "btn-icon-round pressable";
    plus.appendChild(icon("plus"));
    plus.setAttribute("aria-label", "Add a trust");
    plus.onclick = () => {
      this.addFriendOpen = true;
      this.render();
    };
    head.append(h, plus);

    // Appear-offline: the one control on this rail that is about you rather
    // than a conversation. It persists to ui-prefs, which is what tells the
    // daemon to stop heartbeating.
    const offline = uiPrefsSnapshot().appear_offline;
    const offlineRow = document.createElement("div");
    offlineRow.className = `conv-offline${offline ? " on" : ""}`;
    const offlineSwitch = this.makeSwitch(offline);
    offlineSwitch.input.setAttribute("aria-label", "Appear offline");
    offlineSwitch.input.onchange = () =>
      void this.setAppearOffline(offlineSwitch.input.checked);
    const offlineLabel = document.createElement("span");
    offlineLabel.className = "text-13";
    offlineLabel.textContent = offline
      ? "Appearing offline to everyone"
      : "Appear offline";
    offlineRow.append(offlineSwitch.el, offlineLabel);

    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const inp = document.createElement("input");
    inp.placeholder = "Search or press Ctrl+K";
    inp.value = this.search;
    inp.oninput = () => {
      this.search = inp.value;
      this.render();
    };
    search.appendChild(inp);
    const rows = document.createElement("div");
    rows.className = "conv-rows";
    const q = this.search.toLowerCase();
    const list = this.contacts.filter((c) =>
      displayNick(c.nickname).toLowerCase().includes(q),
    );
    if (!list.length) {
      const empty = document.createElement("p");
      empty.className = "muted";
      empty.style.padding = "var(--space-4)";
      empty.textContent = "No chats yet";
      rows.appendChild(empty);
    }
    for (const c of list) {
      const row = document.createElement("button");
      row.className = `conv-row pressable${this.selectedNick === c.nickname ? " selected" : ""}`;
      const av = avatarWithPresence(
        c.protocol_name,
        c.online,
        "sm",
        hasUnread(c.nickname),
        displayNick(c.nickname),
      );
      const meta = document.createElement("div");
      meta.style.flex = "1";
      meta.style.minWidth = "0";
      const name = document.createElement("div");
      name.className = "text-15";
      name.style.fontWeight = "500";
      name.textContent = displayNick(c.nickname);
      const prev = lastPreview(c.nickname);
      const sub = document.createElement("div");
      sub.className = "text-13 muted";
      sub.style.overflow = "hidden";
      sub.style.textOverflow = "ellipsis";
      sub.style.whiteSpace = "nowrap";
      sub.textContent = prev?.text ?? (c.inbound_ready ? "Ready to chat" : "Waiting for them to add you");
      meta.append(name, sub);
      const time = document.createElement("span");
      time.className = "text-12 faint";
      time.textContent = prev ? formatRelative(prev.time) : "";
      row.append(av, meta, time);
      row.onclick = () => {
        this.selectedNick = c.nickname;
        markRead(c.nickname);
        this.render();
      };
      rows.appendChild(row);
    }
    col.append(head, offlineRow, search, rows);
    return col;
  }

  /**
   * Persist appear-offline. The pref is the whole mechanism: the daemon reads
   * it before each heartbeat, so writing it is what makes contacts see this
   * vault offline. Repaint so the capsule reflects the stored value.
   */
  private async setAppearOffline(on: boolean): Promise<void> {
    try {
      await patchUiPrefs({ appear_offline: on });
      // Make the change visible at once rather than waiting out the relay's
      // 90 s freshness window: clear the heartbeat on the way in, restore it on
      // the way out. Both are best-effort — a relay without the clear route
      // still goes offline when the heartbeat stops.
      if (on) await api.presenceOffline().catch(() => {});
      else await api.presenceHeartbeat().catch(() => {});
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Presence could not be changed.",
      );
    }
    this.render();
  }

  private renderMainChat(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.position = "relative";
    const nick = this.selectedNick;
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact || !nick) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      empty.innerHTML = `<svg width="120" height="80" viewBox="0 0 120 80" aria-hidden="true"><ellipse cx="60" cy="50" rx="40" ry="20" fill="var(--accent-soft)"/><circle cx="60" cy="35" r="18" fill="var(--accent)" opacity="0.3"/></svg>`;
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "Quiet in here";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Invite someone to start your first chat";
      const b = document.createElement("button");
      b.className = "btn-primary pressable";
      b.textContent = "Add a trust";
      b.onclick = () => {
        this.addFriendOpen = true;
        this.render();
      };
      empty.append(t, p, b);
      pane.appendChild(empty);
      return pane;
    }

    const header = document.createElement("div");
    header.className = "chat-header";
    const left = document.createElement("div");
    left.style.display = "flex";
    left.style.gap = "var(--space-3)";
    left.style.alignItems = "center";
    left.appendChild(
      avatarWithPresence(contact.protocol_name, contact.online, "lg", false, displayNick(nick)),
    );
    const titles = document.createElement("div");
    const n = document.createElement("div");
    n.className = "text-17";
    n.style.fontWeight = "500";
    n.textContent = displayNick(nick);
    const sub = document.createElement("div");
    sub.className = "text-13 muted";
    sub.style.display = "flex";
    sub.style.alignItems = "center";
    sub.style.gap = "6px";
    sub.style.flexWrap = "wrap";
    const presence = document.createElement("span");
    presence.className = contact.online ? "presence-label online" : "presence-label";
    presence.textContent = contact.online ? "Online" : "Offline";
    sub.append(presence, document.createTextNode("·"));
    const lockLine = document.createElement("span");
    lockLine.style.display = "inline-flex";
    lockLine.style.alignItems = "center";
    lockLine.style.gap = "4px";
    lockLine.append(icon("lock"), document.createTextNode("End-to-end encrypted"));
    sub.append(lockLine);
    titles.append(n, sub);
    left.append(titles);
    // The conversation's actions, in one place: who this is, whether it is
    // muted, and the one destructive action. The profile dialog is now only
    // about the name, so the two do not repeat each other.
    const menu = new OverflowMenu({
      label: `Conversation options for ${displayNick(nick)}`,
      items: [
        {
          id: "rename",
          label: "Change nickname",
          icon: "pencil",
          onSelect: () => {
            this.profileNick = nick;
            this.render();
          },
        },
        {
          id: "mute",
          label: "Mute notifications",
          icon: isChatSoundMuted(nick) ? "bellOff" : "bell",
          checked: isChatSoundMuted(nick),
          onSelect: () => {
            void setChatSoundMuted(nick, !isChatSoundMuted(nick)).then(() =>
              this.render(),
            );
          },
        },
        {
          id: "delete",
          label: "Delete contact",
          icon: "trash",
          danger: true,
          separatorBefore: true,
          onSelect: () => this.openTrustDelete(nick),
        },
      ],
    });
    header.append(left, menu.element);

    const scroll = document.createElement("div");
    scroll.className = "chat-messages";
    const msgs = getThread(nick);
    let lastDay = "";
    for (const group of groupMessages(msgs)) {
      if (group.day !== lastDay) {
        lastDay = group.day;
        const sep = document.createElement("div");
        sep.className = "day-sep";
        sep.textContent = group.day;
        scroll.appendChild(sep);
      }
      const gEl = document.createElement("div");
      gEl.className = `bubble-group ${group.outgoing ? "out" : "in"}`;
      group.messages.forEach((m, i) => {
        const tail = i === group.messages.length - 1;
        const b = document.createElement("div");
        b.className = `bubble ${group.outgoing ? "out" : "in"}${tail ? " tail" : ""}`;
        if (m.kind === "text") b.textContent = m.text ?? "";
        else {
          const fc = document.createElement("div");
          fc.className = "file-card";
          fc.innerHTML = `<div class="text-15" style="font-weight:500">${m.fileName}</div><div class="text-13 muted">${formatBytes(m.fileSize ?? 0)}</div><div class="text-12" style="color:var(--success-text);background:var(--success-soft);display:inline-block;padding:2px 8px;border-radius:var(--radius-pill);margin-top:8px">Sealed on your device</div>`;
          if (m.sendProgress !== undefined) {
            const bar = document.createElement("div");
            bar.style.height = "3px";
            bar.style.background = "var(--border)";
            bar.style.marginTop = "8px";
            bar.style.borderRadius = "2px";
            const fill = document.createElement("div");
            fill.style.height = "100%";
            fill.style.width = `${m.sendProgress}%`;
            fill.style.background = "var(--accent)";
            bar.appendChild(fill);
            fc.appendChild(bar);
          }
          const uploading =
            m.sendProgress !== undefined && m.sendProgress < 100;
          if (!uploading && (m.fileData || m.blobId)) {
            attachFileActions(
              fc,
              m.fileName ?? "file",
              () => resolveMessageFileBytes(m),
              m.blobId,
            );
          } else if (!uploading && !m.fileData && !m.blobId) {
            const hint = document.createElement("p");
            hint.className = "text-12 muted";
            hint.style.marginTop = "var(--space-2)";
            hint.textContent =
              "File bytes are not on this device. Ask your contact to send again.";
            fc.appendChild(hint);
          }
          b.appendChild(fc);
        }
        if (m.reactions.length) {
          const pills = document.createElement("div");
          pills.style.display = "flex";
          pills.style.gap = "4px";
          pills.style.marginTop = "4px";
          m.reactions.forEach((r) => {
            const p = document.createElement("span");
            p.className = "text-12";
            p.style.background = "var(--surface)";
            p.style.padding = "2px 6px";
            p.style.borderRadius = "var(--radius-pill)";
            p.textContent = r;
            pills.appendChild(p);
          });
          b.appendChild(pills);
        }
        b.onmouseenter = () => {
          /* reaction bar simplified: double-click heart */
        };
        b.ondblclick = () => {
          toggleReaction(m.id, "❤");
          this.render();
        };
        gEl.appendChild(b);
      });
      scroll.appendChild(gEl);
    }
    // Follow new messages only while the reader is already at the bottom;
    // someone scrolled up reading history stays where they are. A position
    // from a DIFFERENT conversation is not restored: opening a chat always
    // lands on the latest message. Applied after attachment (see render).
    const prev = this.chatScrollPos;
    this.chatScrollPos = null;
    if (this.bottomOnNextChatRender) {
      this.bottomOnNextChatRender = false;
      this.pendingChatScroll = { nick, mode: "bottom" };
    } else if (!prev || prev.nick !== nick || prev.atBottom) {
      this.pendingChatScroll = { nick, mode: "bottom" };
    } else {
      this.pendingChatScroll = { nick, mode: "restore", top: prev.top };
    }

    const composer = this.renderComposer(nick, contact.online);
    pane.append(header, scroll, composer);

    pane.ondragover = (e) => {
      e.preventDefault();
    };
    pane.ondrop = async (e) => {
      e.preventDefault();
      const f = e.dataTransfer?.files?.[0];
      if (f) await this.sendFile(nick, f);
    };

    return pane;
  }

  private renderComposer(nick: string, contactOnline: boolean): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "composer";
    if (!contactOnline) {
      const hint = document.createElement("p");
      hint.className = "text-13 muted composer-offline-hint";
      hint.textContent =
        `${displayNick(nick)} is offline. Chat and files need you both online — use Mail to reach them later.`;
      wrap.appendChild(hint);
    }
    const inner = document.createElement("div");
    inner.className = "composer-inner";
    if (!contactOnline) inner.classList.add("composer-disabled");
    const attach = document.createElement("button");
    attach.className = "pressable";
    attach.appendChild(icon("attach"));
    const fileIn = document.createElement("input");
    fileIn.type = "file";
    fileIn.hidden = true;
    attach.onclick = () => fileIn.click();
    fileIn.onchange = async () => {
      const f = fileIn.files?.[0];
      if (f) await this.sendFile(nick, f);
      fileIn.value = "";
    };
    const ta = document.createElement("textarea");
    ta.rows = 1;
    ta.placeholder = contactOnline ? "Message" : "Waiting for them to come online…";
    ta.disabled = !contactOnline;
    const send = document.createElement("button");
    send.className = "send-fab btn-icon-round pressable";
    send.appendChild(icon("send"));
    const syncSend = () => {
      send.classList.toggle("ready", ta.value.trim().length > 0);
    };
    ta.oninput = () => {
      syncSend();
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight}px`;
    };
    ta.onkeydown = (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void this.sendText(nick, ta.value.trim(), ta, send);
      }
    };
    send.disabled = !contactOnline;
    send.onclick = () => void this.sendText(nick, ta.value.trim(), ta, send);
    attach.disabled = !contactOnline;
    inner.append(attach, fileIn, ta, send);
    wrap.appendChild(inner);
    syncSend();
    return wrap;
  }

  private async sendText(nick: string, text: string, ta: HTMLTextAreaElement, send: HTMLButtonElement): Promise<void> {
    if (!text) return;
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact?.online) {
      this.announce(`${displayNick(nick)} is offline. Chat needs you both online — send Mail instead.`);
      return;
    }
    const msg = addOutgoingText(nick, text);
    ta.value = "";
    send.classList.remove("ready");
    this.render();
    try {
      await api.sendText(nick, text);
    } catch (e) {
      removeOutgoingMessage(nick, msg.id);
      this.announce(e instanceof AppError ? e.message : "Couldn't send. Try again.");
      this.render();
    }
  }

  private async sendFile(nick: string, file: File): Promise<void> {
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact?.online) {
      this.announce(`${displayNick(nick)} is offline. Chat needs you both online — send Mail instead.`);
      return;
    }
    if (file.size > MAX_FILE_BYTES) {
      this.announce(`Files are limited to ${Math.round(MAX_FILE_BYTES / 1024)} KB for now`);
      return;
    }
    const buf = new Uint8Array(await file.arrayBuffer());
    const msg = addOutgoingFile(nick, file.name, file.size, buf);
    this.render();
    try {
      for (let p = 20; p <= 100; p += 20) {
        setSendProgress(msg.id, p);
        this.render();
        await sleep(80);
      }
      const b64 = await fileToBase64(file);
      const resp = await api.sendFileData(nick, file.name, b64);
      if (resp?.blob_id) setMessageBlobId(nick, msg.id, resp.blob_id);
      clearSendProgress(nick, msg.id);
      this.render();
    } catch (e) {
      removeOutgoingMessage(nick, msg.id);
      this.announce(e instanceof AppError ? e.message : "Couldn't send. Try again.");
      this.render();
    }
  }

  // ---------------------------------------------------------------- drops
  //
  // Drops are external anonymous-submission sites, usually SecureDrop. Hush
  // never proxies them: the shell opens a dedicated, capability-less window
  // whose WebKit context is pointed at a local Tor SOCKS port. This rail is the
  // directory and the status surface around that window — nothing here talks to
  // the Hush relay about a Drop.

  private startDropsPoll(): void {
    if (this.dropsPollTimer !== null) return;
    this.dropsPollTimer = window.setInterval(() => void this.refreshDrops(), 2500);
  }

  /**
   * Poll the shell's Tor while the app is unlocked. The pill repaints in
   * place — a full render on every tick would rebuild the whole shell twice
   * a second for a status nobody is looking at.
   */
  private startTorPoll(): void {
    if (this.torPillTimer !== null) return;
    this.torPillTimer = window.setInterval(() => void this.refreshTorPill(), 2500);
    void this.refreshTorPill();
  }

  private stopTorPoll(): void {
    if (this.torPillTimer !== null) clearInterval(this.torPillTimer);
    this.torPillTimer = null;
  }

  private async refreshTorPill(): Promise<void> {
    const [tor, cfg] = await Promise.all([
      torStatus(),
      api.relayConfig().catch(() => null),
    ]);
    const mode = cfg?.tor_mode ?? false;
    const applied = cfg?.applied ?? false;
    const onion = cfg?.onion_url ?? null;
    const reachable = cfg?.reachable ?? null;
    const changed =
      tor.phase !== this.torPhase ||
      tor.message !== this.torMessage ||
      mode !== this.torMode ||
      applied !== this.torApplied ||
      onion !== this.torOnionUrl ||
      reachable !== this.torReachable;
    this.torPhase = tor.phase;
    this.torMessage = tor.message;
    this.torMode = mode;
    this.torApplied = applied;
    this.torOnionUrl = onion;
    this.torReachable = reachable;
    if (changed) this.updateTorPill();

    // The wallet's Tor is this same shell process, so it heals on this poll
    // too. A daemon restart wipes the port it was given, and a fresh Tor gets a
    // fresh one: either way the wallet would be left with no route, which in
    // Tor mode is a refusal. Announce the live port, and bring Tor up if the
    // user chose Tor for the wallet and it is not running.
    const wallet = getWalletState();
    if (wallet.exists && wallet.mode === "tor") {
      if (tor.phase === "ready" && tor.socks_port) {
        const live = `socks5h://127.0.0.1:${tor.socks_port}`;
        if (this.walletProxy !== live) {
          try {
            await setWalletProxy(live);
            this.walletProxy = live;
          } catch {
            /* the next poll retries */
          }
        }
      } else if (tor.phase === "stopped" && !this.walletTorBusy) {
        this.walletTorBusy = true;
        void torStart().finally(() => {
          this.walletTorBusy = false;
        });
      }
    }

    // The switch being on is the whole instruction — nothing else is asked
    // of the user, ever. Two ways that can be true while traffic is not yet
    // protected, and both heal here on their own:
    // - the shell's Tor is up but the daemon holds a different (or no) SOCKS
    //   port — a daemon restart wipes it, and a fresh Tor gets a fresh port.
    //   Post the live port; the daemon applies it to the vault.
    // - the shell's Tor is not running at all (fresh start with the switch
    //   already on). Boot it; the port sync lands on a later poll.
    if (!mode || !onion) return;
    if (tor.phase === "ready" && tor.socks_port) {
      const live = `socks5h://127.0.0.1:${tor.socks_port}`;
      if (cfg && cfg.socks !== live) {
        try {
          await api.setRelayMode(true, live);
          // The daemon applied it synchronously; skip the pending flicker.
          this.torApplied = true;
          this.updateTorPill();
        } catch {
          /* the next poll retries */
        }
      }
    } else if (tor.phase === "stopped" && !this.torAutoBusy) {
      this.torAutoBusy = true;
      void torStart().finally(() => {
        this.torAutoBusy = false;
      });
    }
  }

  /** The shield's state and its accessible name. Green covers both
   * connection paths — clearnet and Tor — and the name says which; orange is
   * "coming up"; red is "not connected". Reachability, not Tor's phase, is
   * what makes red correct: Tor stays "ready" long after the network is gone,
   * so only the daemon's probe can tell the truth. */
  private relayShield(): { cls: string; alt: string } {
    if (!this.torMode) {
      if (this.torReachable === false) {
        return { cls: "relay-offline", alt: "Hush is disconnected." };
      }
      return { cls: "relay-cleartext", alt: "Hush is connected through Clearnet." };
    }
    if (this.torPhase === "failed" || this.torPhase === "unavailable") {
      return { cls: "relay-offline", alt: "Hush is disconnected." };
    }
    if (this.torPhase !== "ready") {
      return { cls: "relay-connecting", alt: "Hush is trying to connect." };
    }
    // Tor is up; the relay is only reachable if the probe says so.
    if (this.torReachable === false) {
      return { cls: "relay-offline", alt: "Hush is disconnected." };
    }
    return { cls: "relay-tor", alt: "Hush is connected through Tor." };
  }

  private updateTorPill(): void {
    const el = this.root.querySelector<HTMLElement>(".relay-shield");
    if (el) this.updateTorPillInto(el);
  }

  private updateTorPillInto(el: HTMLElement): void {
    const { cls, alt } = this.relayShield();
    el.classList.remove(
      "relay-cleartext",
      "relay-tor",
      "relay-connecting",
      "relay-offline",
    );
    el.classList.add(cls);
    // The words the user asked for, as the icon's accessible name and its
    // tooltip.
    el.setAttribute("aria-label", alt);
    el.title = alt;
  }

  /** Flip the relay switch. The onion address is not asked for here — it is
   * once-per-vault setup that lives in Settings. Never falls back: with the
   * switch on and Tor down, the UI says so and mail queues. Throws so each
   * surface can show the failure next to its own switch. */
  private async toggleTorRelay(on: boolean): Promise<void> {
    this.torActionError = null;
    if (!on) {
      await api.setRelayMode(false, null);
      // Tor serves the Drop window and the wallet too; stop it only when
      // nothing else needs it.
      const open = await openState();
      const wallet = getWalletState();
      const walletWantsTor = wallet.exists && wallet.mode === "tor";
      if (!open.open && !walletWantsTor) await torStop();
      await this.refreshStatus();
      this.render();
      return;
    }
    const st = await torStart();
    if (st.phase !== "ready" || !st.socks_port) {
      throw new AppError(
        st.message ?? "Tor could not connect. The switch is still off.",
      );
    }
    await api.setRelayMode(true, `socks5h://127.0.0.1:${st.socks_port}`);
    await this.refreshStatus();
    this.render();
  }

  private stopDropsPoll(): void {
    if (this.dropsPollTimer !== null) clearInterval(this.dropsPollTimer);
    this.dropsPollTimer = null;
  }

  /** Pull Tor and open-window state; repaint only when something changed. */
  private async refreshDrops(): Promise<void> {
    const [tor, open, dev] = await Promise.all([
      torStatus(),
      openState(),
      devEnabled(),
    ]);
    const openId = open.open ? open.id : null;
    const changed =
      tor.phase !== this.dropsTor.phase ||
      tor.message !== this.dropsTor.message ||
      openId !== this.dropsOpenId ||
      dev !== this.dropsDevAllowed;
    this.dropsTor = tor;
    this.dropsOpenId = openId;
    this.dropsDevAllowed = dev;
    if (changed && this.rail === "drops") this.render();
  }

  /** Bring Tor up explicitly and surface the reason if it cannot. */
  private async connectTor(): Promise<void> {
    if (this.dropsTorBusy) return;
    this.dropsTorBusy = true;
    this.dropsError = null;
    this.render();
    try {
      const status = await torStart(120_000);
      this.dropsTor = status;
      if (status.phase === "ready") {
        this.announce("Tor connected.");
      } else if (status.message) {
        this.dropsError = status.message;
      }
    } finally {
      this.dropsTorBusy = false;
      if (this.rail === "drops") this.render();
    }
  }

  private renderDrops(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane drops-pane";

    const head = document.createElement("div");
    head.className = "trusts-head drops-head";
    const titles = document.createElement("div");
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Drops";
    const sub = document.createElement("p");
    sub.className = "muted text-13";
    sub.style.marginTop = "var(--space-2)";
    const count = verifiedCount();
    sub.textContent = count
      ? `${count} verified ${
          count === 1 ? "destination" : "destinations"
        }. Opened from this device through Tor — the Hush relay is not used.`
      : "Anonymous submission channels. Opened from this device through Tor — the Hush relay is not used.";
    titles.append(h, sub);
    const status = document.createElement("div");
    status.className = "drops-head-status";
    status.appendChild(this.dropsTorChip());
    head.append(titles, status);
    pane.appendChild(head);

    if (this.dropsOpenId) {
      const open = dropById(this.dropsOpenId);
      const banner = document.createElement("div");
      banner.className = "drops-open-banner";
      const dot = document.createElement("span");
      dot.className = `drops-tor-dot ${this.dropsTor.phase}`;
      const text = document.createElement("div");
      text.className = "drops-open-text";
      const line1 = document.createElement("div");
      line1.className = "text-15";
      line1.textContent = open
        ? `${open.name} is open in its own window.`
        : "A Drop is open in its own window.";
      const line2 = document.createElement("div");
      line2.className = "muted text-13";
      line2.textContent =
        "Traffic to this Drop is routed through Tor. Hush relay is not used.";
      text.append(line1, line2);
      const close = document.createElement("button");
      close.type = "button";
      close.className = "btn-secondary pressable";
      close.textContent = "Close Drop";
      close.onclick = () => void this.closeOpenDrop();
      banner.append(dot, text, close);
      pane.appendChild(banner);
    }

    if (this.dropsError) {
      const err = document.createElement("p");
      err.className = "drops-error";
      err.textContent = this.dropsError;
      pane.appendChild(err);
    }

    const controls = document.createElement("div");
    controls.className = "drops-controls";

    const unverified = document.createElement("label");
    unverified.className = "drops-toggle text-13";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = this.dropsShowUnverified;
    cb.onchange = () => {
      this.dropsShowUnverified = cb.checked;
      this.render();
    };
    unverified.append(cb, document.createTextNode("Show unverified entries"));
    controls.appendChild(unverified);

    if (this.dropsDevAllowed) {
      const dev = document.createElement("span");
      dev.className = "drops-dev-note text-12";
      dev.textContent = "Development Drops enabled (HUSH_DROPS_DEV=1)";
      controls.appendChild(dev);
    }

    if (!dropsAvailable()) {
      const note = document.createElement("span");
      note.className = "drops-dev-note text-12";
      note.textContent = "Open Drops in the Hush desktop app";
      controls.appendChild(note);
    } else if (this.dropsTor.phase !== "ready") {
      // Tor starts on demand when a real Drop opens; this button exists so the
      // connection can be brought up and seen before that, and retried after a
      // failure.
      const connect = document.createElement("button");
      connect.type = "button";
      connect.className = "btn-secondary pressable";
      connect.disabled = this.dropsTorBusy;
      connect.textContent = this.dropsTorBusy
        ? "Connecting to Tor…"
        : "Connect Tor";
      connect.onclick = () => void this.connectTor();
      controls.appendChild(connect);
    }

    const refresh = document.createElement("button");
    refresh.type = "button";
    refresh.className = "drops-refresh pressable";
    refresh.setAttribute("aria-label", "Refresh Tor status");
    refresh.title = "Refresh Tor status";
    refresh.appendChild(icon("refresh"));
    refresh.onclick = () => void this.refreshDrops();
    controls.appendChild(refresh);
    pane.appendChild(controls);

    const drops = visibleDrops({
      includeUnverified: this.dropsShowUnverified,
      includeDev: this.dropsDevAllowed,
    });

    if (drops.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "No verified Drops yet";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent =
        "Verified SecureDrop destinations appear here once their onion address is confirmed against a trusted source.";
      empty.append(t, p);
      pane.append(empty, this.dropsFootnote());
      return pane;
    }

    const grid = document.createElement("div");
    grid.className = "bento drops-grid";
    for (const d of drops) grid.appendChild(this.dropCard(d));
    pane.append(grid, this.dropsFootnote());
    return pane;
  }

  /** One Drop as a card: an app tile with its own identity, not a person. */
  private dropCard(d: Drop): HTMLElement {
    const card = document.createElement("div");
    card.className = `tile drop-card${d.devOnly ? " is-dev" : ""}`;

    const top = document.createElement("div");
    top.className = "drop-card-top";
    top.appendChild(this.dropAvatar(d));

    const verified = d.verified && !d.devOnly;
    const badge = document.createElement("span");
    badge.className = `drop-badge ${
      d.devOnly ? "dev" : verified ? "verified" : "unverified"
    }`;
    badge.textContent = d.devOnly ? "Test" : verified ? "✓ Verified" : "⚠ Unverified";
    top.appendChild(badge);

    // Name and org sit on their own full-width lines under the avatar row so
    // long names are never cut off next to the icon.
    const meta = document.createElement("div");
    meta.className = "drop-card-meta";
    const name = document.createElement("div");
    name.className = "text-17 drop-card-name";
    name.textContent = d.name;
    const org = document.createElement("div");
    org.className = "muted text-13 drop-card-org";
    org.textContent = d.organization;
    meta.append(name, org);

    const desc = document.createElement("p");
    desc.className = "muted text-13 drop-card-desc";
    desc.textContent = d.description;

    const facts = document.createElement("div");
    facts.className = "drop-facts text-12";
    const fact = (ic: IconName, text: string): HTMLElement => {
      const s = document.createElement("span");
      s.className = "drop-fact";
      s.append(icon(ic), document.createTextNode(text));
      return s;
    };
    facts.append(
      fact("globe", d.country),
      fact("shieldCheck", `Protocol: ${d.protocol}`),
      fact("files", d.category),
    );

    const verify = document.createElement("div");
    verify.className = "drop-verify text-12";
    verify.textContent = verificationDetail(d);

    const torRow = document.createElement("div");
    torRow.className = "drop-tor-row";
    torRow.appendChild(this.dropsTorChip());

    const open = document.createElement("button");
    open.type = "button";
    open.className = "btn-primary pressable drop-open";
    open.appendChild(icon("externalLink"));
    const openLabel = document.createElement("span");
    const busy = this.dropsOpeningId !== null;
    openLabel.textContent = this.dropsOpeningId === d.id ? "Opening…" : "Open Drop";
    open.appendChild(openLabel);
    const canOpen = isOpenable(d) && !busy;
    open.disabled = !canOpen;
    if (!isOpenable(d)) open.title = verificationDetail(d);
    open.onclick = () => void this.openDropCard(d);

    card.append(top, meta, desc, facts, verify, torRow, open);
    return card;
  }

  /** Initials avatar in the same visual family as the contact avatars. */
  private dropAvatar(d: Drop): HTMLElement {
    const el = document.createElement("div");
    el.className = "drop-avatar";
    const hue = hueFromKey(d.id);
    el.style.background = `linear-gradient(135deg, hsl(${hue},52%,60%), hsl(${
      (hue + 40) % 360
    },46%,50%))`;
    el.textContent = initialsFor(d.name) || "?";
    return el;
  }

  /** The Tor status pill, reused on the rail header, cards, and the banner. */
  private dropsTorChip(): HTMLElement {
    const shell = dropsAvailable();
    const phase = this.dropsTor.phase;
    const chip = document.createElement("span");
    chip.className = `drops-tor-chip ${shell ? phase : "unavailable"}`;
    const dot = document.createElement("span");
    dot.className = "drops-tor-dot";
    const label = document.createElement("span");
    // Outside the desktop shell there is no Tor at all, so say that rather than
    // imply a connection that failed.
    label.textContent = shell ? torPhaseLabel(phase) : "Desktop app required";
    chip.append(dot, label);
    if (!shell) {
      chip.title =
        "Drops run Tor from the Hush desktop app, not from a browser tab.";
    } else if (
      this.dropsTor.message &&
      (phase === "failed" || phase === "unavailable")
    ) {
      chip.title = this.dropsTor.message;
    }
    return chip;
  }

  private dropsFootnote(): HTMLElement {
    const p = document.createElement("p");
    p.className = "drops-footnote faint text-12";
    p.textContent =
      "Tor hides the destination from your local network. It does not make a submission anonymous by itself — follow the Drop's own guidance.";
    return p;
  }

  private async openDropCard(d: Drop): Promise<void> {
    if (this.dropsOpeningId) return;
    this.dropsOpeningId = d.id;
    this.dropsError = null;
    this.render();
    try {
      if (!dropsAvailable()) {
        throw new Error("Opening Drops needs the Hush desktop app.");
      }
      await openDrop(d);
      this.dropsOpenId = d.id;
      this.announce(`${d.name} opened. Traffic is routed through Tor.`);
    } catch (e) {
      // The shell returns plain-language reasons; show them as they are.
      this.dropsError = e instanceof Error ? e.message : String(e);
    } finally {
      this.dropsOpeningId = null;
      if (this.rail === "drops") this.render();
    }
  }

  private async closeOpenDrop(): Promise<void> {
    try {
      await closeDrop();
    } catch (e) {
      this.dropsError = e instanceof Error ? e.message : String(e);
    }
    this.dropsOpenId = null;
    if (this.rail === "drops") this.render();
  }

  // --------------------------------------------------------------- trusts

  /**
   * The Trusts rail: the contact list, one person per row.
   *
   * Rows speak the same visual language as the Chats rail's list — a person is
   * a row there, so a person is a row here. The actions live in an overflow
   * menu, so a long list stays scannable instead of repeating a button row
   * down the pane. Adding an action is one menu entry, not a layout change.
   */
  private renderTrusts(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane trusts-pane";

    const head = document.createElement("div");
    head.className = "trusts-head";
    const titles = document.createElement("div");
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Trusts";
    const sub = document.createElement("p");
    sub.className = "muted text-13";
    sub.style.marginTop = "var(--space-2)";
    sub.textContent = `${this.contacts.length} ${
      this.contacts.length === 1 ? "contact" : "contacts"
    }`;
    titles.append(h, sub);
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-primary pressable trusts-add";
    add.textContent = "Add a trust";
    add.onclick = () => {
      this.addFriendOpen = true;
      this.render();
    };
    head.append(titles, add);
    pane.appendChild(head);

    // A filter over the cards, so a long list stays findable. It toggles the
    // rendered cards directly instead of re-rendering, so typing never loses
    // focus.
    const filter = document.createElement("div");
    filter.className = "search-pill trusts-filter";
    const filterInput = document.createElement("input");
    filterInput.type = "search";
    filterInput.placeholder = "Filter contacts";
    filterInput.setAttribute("aria-label", "Filter contacts");
    filter.append(icon("search"), filterInput);

    if (!this.contacts.length) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "No contacts yet";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Add someone to start a chat or send mail.";
      const b = document.createElement("button");
      b.className = "btn-primary pressable";
      b.textContent = "Add a trust";
      b.onclick = () => {
        this.addFriendOpen = true;
        this.render();
      };
      empty.append(t, p, b);
      pane.appendChild(empty);
      return pane;
    }

    // Ordered by the name the card actually shows, so the grid stays scannable
    // as it grows. Sorted on a copy: the contact array's order is not ours to
    // change, and a comparison by nickname could not see the decoration.
    const ordered = [...this.contacts].sort((a, b) =>
      displayNick(a.nickname).localeCompare(displayNick(b.nickname), undefined, {
        sensitivity: "base",
      }),
    );
    const rows = document.createElement("div");
    rows.className = "trust-rows";
    for (const c of ordered) rows.appendChild(this.trustRow(c));

    const noMatch = document.createElement("p");
    noMatch.className = "muted text-13 trusts-filter-empty";
    noMatch.hidden = true;
    noMatch.textContent = "No contacts match that filter.";

    filterInput.oninput = () => {
      const q = filterInput.value.trim().toLocaleLowerCase();
      let shown = 0;
      for (const el of Array.from(rows.children) as HTMLElement[]) {
        const match =
          !q || (el.dataset.trustName ?? "").toLocaleLowerCase().includes(q);
        el.hidden = !match;
        if (match) shown++;
      }
      noMatch.hidden = shown > 0;
    };

    pane.append(filter, rows, noMatch);
    return pane;
  }

  /**
   * One contact as a row: avatar with presence, the names they go by, and the
   * actions in an overflow menu. The row has one action — its menu. Clicking
   * anywhere on the row opens it, as a right-click would on a list item, and
   * the menu carries chat, mail and the destructive ones, so the row never
   * has to guess which action a click meant.
   */
  private trustRow(c: ContactDto): HTMLElement {
    const row = document.createElement("div");
    row.className = "trust-row pressable";
    // The name as shown, so the filter matches what the user can see.
    row.dataset.trustName = displayNick(c.nickname);

    row.appendChild(
      avatarWithPresence(c.protocol_name, c.online, "sm", false, displayNick(c.nickname)),
    );

    const main = document.createElement("div");
    main.className = "trust-row-main";
    const name = document.createElement("div");
    name.className = "text-15 trust-row-name";
    name.textContent = displayNick(c.nickname);
    main.appendChild(name);
    if (c.mail_handle) {
      const mail = document.createElement("div");
      mail.className = "muted text-13 trust-row-mail";
      mail.textContent = this.contactAddress(c);
      main.appendChild(mail);
    }
    row.appendChild(main);

    // The extensible action menu, nested so the popup anchors to the dots.
    // Each action is an entry, not a layout change, and the destructive one is
    // last and separated: a delete that sits beside a primary action is a
    // delete that gets hit by accident. The row is the menu's surface, so a
    // second press on the row closes what the first one opened; clicks that
    // begin inside the menu toggle on their own and are not re-toggled here.
    const menu = new OverflowMenu({
      label: `Contact options for ${displayNick(c.nickname)}`,
      surface: row,
      items: [
        {
          id: "chat",
          label: "Chat",
          icon: "chats",
          onSelect: () => this.openChat(c.nickname),
        },
        {
          id: "email",
          label: "Email",
          icon: "mail",
          onSelect: () => this.emailContact(c.nickname),
        },
        {
          id: "rename",
          label: "Change nickname",
          icon: "pencil",
          onSelect: () => {
            this.profileNick = c.nickname;
            this.render();
          },
        },
        {
          id: "delete",
          label: "Delete",
          icon: "trash",
          danger: true,
          separatorBefore: true,
          onSelect: () => this.openTrustDelete(c.nickname),
        },
      ],
    });
    row.appendChild(menu.element);
    row.onclick = (e) => {
      if (menu.element.contains(e.target as Node)) return;
      menu.toggleMenu();
    };
    return row;
  }

  /** Open the chat thread with a contact, from any rail. */
  private openChat(nick: string): void {
    this.rail = "chats";
    this.selectedNick = nick;
    markRead(nick);
    this.render();
  }

  /** Open the mail composer with a contact already chosen as the recipient. */
  private emailContact(nick: string): void {
    this.rail = "mail";
    this.openCompose({ to: nick });
  }

  // ---------------------------------------------------------------------
  // Conclaves
  //
  // A conclave is a local record in the vault, not a relay-side room: sending in
  // one fans the message out to each member over their own 1:1 session. The
  // daemon owns the record and the fan-out; this rail renders the daemon's view
  // and drives create, send, and the local membership changes.
  // ---------------------------------------------------------------------

  private renderConclaves(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "conclaves-pane";
    pane.appendChild(this.renderConclavesList());
    pane.appendChild(this.renderConclaveDetail());
    return pane;
  }

  /** The left column: a filter over conclave cards, newest first. */
  private renderConclavesList(): HTMLElement {
    const col = document.createElement("div");
    col.className = "conclaves-list";

    const head = document.createElement("div");
    head.className = "conclaves-list-head";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Conclaves";
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-primary pressable conclaves-new";
    add.textContent = "New conclave";
    add.disabled = this.contacts.length === 0;
    add.title = this.contacts.length
      ? "Create a conclave"
      : "Add a trust before you can meet with anyone";
    add.onclick = () => this.openConclaveCreate();
    head.append(h, add);
    col.appendChild(head);

    const conclaves = listConclaves();
    const invites = listPendingInvites();
    if (!conclaves.length && !invites.length) {
      const empty = document.createElement("div");
      empty.className = "empty-state conclaves-empty";
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "No conclaves yet";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = this.contacts.length
        ? "Start a conclave and bring a few people in."
        : "Add a trust, then you can meet with them.";
      empty.append(t, p);
      if (this.contacts.length) {
        const b = document.createElement("button");
        b.className = "btn-primary pressable";
        b.textContent = "New conclave";
        b.onclick = () => this.openConclaveCreate();
        empty.appendChild(b);
      }
      col.appendChild(empty);
      return col;
    }

    const filter = document.createElement("div");
    filter.className = "search-pill conclaves-filter";
    const filterInput = document.createElement("input");
    filterInput.type = "search";
    filterInput.placeholder = "Filter conclaves";
    filterInput.setAttribute("aria-label", "Filter conclaves");
    filter.append(icon("search"), filterInput);
    col.appendChild(filter);

    // Invitations waiting on an accept or decline, above the conclave cards. A
    // pending invite is not a conclave yet: nothing is materialised until it is
    // accepted.
    if (invites.length) {
      const box = document.createElement("div");
      box.className = "conclave-invites";
      for (const invite of invites) {
        const card = document.createElement("div");
        card.className = "conclave-invite-card";
        const title = document.createElement("div");
        title.className = "text-15";
        title.textContent = invite.name;
        const from = document.createElement("div");
        from.className = "muted text-12";
        from.textContent = `${displayNick(invite.fromNick)} invited you`;
        const row = document.createElement("div");
        row.className = "conclave-invite-actions";
        const accept = document.createElement("button");
        accept.type = "button";
        accept.className = "btn-primary pressable";
        accept.textContent = "Accept";
        accept.onclick = () => void this.acceptConclaveInvite(invite.conclaveId);
        const decline = document.createElement("button");
        decline.type = "button";
        decline.className = "btn-secondary pressable";
        decline.textContent = "Decline";
        decline.onclick = () => void this.declineConclaveInvite(invite.conclaveId);
        row.append(accept, decline);
        card.append(title, from, row);
        box.appendChild(card);
      }
      col.appendChild(box);
    }

    const cards = document.createElement("div");
    cards.className = "conclaves-cards";
    for (const m of conclaves) cards.appendChild(this.conclaveCard(m));

    const noMatch = document.createElement("p");
    noMatch.className = "muted text-13 conclaves-filter-empty";
    noMatch.hidden = true;
    noMatch.textContent = "No conclaves match that filter.";
    filterInput.oninput = () => {
      const q = filterInput.value.trim().toLocaleLowerCase();
      let shown = 0;
      for (const el of Array.from(cards.children) as HTMLElement[]) {
        const match =
          !q || (el.dataset.conclaveName ?? "").toLocaleLowerCase().includes(q);
        el.hidden = !match;
        if (match) shown++;
      }
      noMatch.hidden = shown > 0;
    };

    col.append(cards, noMatch);
    return col;
  }

  /** One conclave card: name, member count, and a preview of the last message. */
  private conclaveCard(m: Conclave): HTMLElement {
    const card = document.createElement("button");
    card.type = "button";
    card.className = `tile conclave-card pressable${
      this.conclaveId === m.id ? " selected" : ""
    }`;
    card.dataset.conclaveName = m.name;

    const top = document.createElement("div");
    top.className = "conclave-card-top";
    const badge = document.createElement("span");
    badge.className = "conclave-card-icon";
    badge.appendChild(icon("conclaves"));
    const meta = document.createElement("div");
    meta.className = "conclave-card-meta";
    const name = document.createElement("div");
    name.className = "text-17 conclave-card-name";
    name.textContent = m.name;
    const sub = document.createElement("div");
    sub.className = "muted text-12";
    const joined = joinedTrustees(m).length;
    sub.textContent = m.ended
      ? "Ended"
      : `${joined} ${joined === 1 ? "trustee" : "trustees"}`;
    meta.append(name, sub);
    top.append(badge, meta);
    card.appendChild(top);

    const preview = document.createElement("div");
    preview.className = "conclave-card-preview muted text-13";
    preview.textContent = conclavePreview(m);
    card.appendChild(preview);

    card.onclick = () => {
      if (this.conclaveId !== m.id) {
        // A different conclave's paper must never bleed into this one's tab.
        this.leavePaperTab();
      }
      this.conclaveId = m.id;
      this.render();
    };
    return card;
  }

  /** The right column: the selected conclave's members and group chat. */
  private renderConclaveDetail(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "conclaves-detail";
    const conclave = this.conclaveId ? conclaveById(this.conclaveId) : undefined;
    if (!conclave) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "Pick a conclave";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Choose a conclave on the left, or start a new one.";
      empty.append(t, p);
      pane.appendChild(empty);
      return pane;
    }

    const me = this.status?.protocol_name ?? "";
    const isConvener = conclave.convener === me;
    const joinedCount = joinedTrustees(conclave).length;
    const invitedCount = invitedTrustees(conclave).length;

    const header = document.createElement("div");
    header.className = "conclave-header";
    const titles = document.createElement("div");
    const n = document.createElement("div");
    n.className = "text-17";
    n.style.fontWeight = "500";
    n.textContent = conclave.name;
    const sub = document.createElement("div");
    sub.className = "text-13 muted";
    sub.textContent = conclave.ended
      ? "Ended — read only"
      : `${joinedCount} ${joinedCount === 1 ? "trustee" : "trustees"}${
          invitedCount ? ` · ${invitedCount} invited` : ""
        }`;
    titles.append(n, sub);

    const actions = document.createElement("div");
    actions.className = "conclave-header-actions";
    if (conclave.ended) {
      // Read-only everywhere: no invite, no leave, no send. Deleting the
      // ended record and its history is the one thing left to do, and it is
      // only this vault's copy.
      const deleteBtn = document.createElement("button");
      deleteBtn.type = "button";
      deleteBtn.className = "btn-secondary pressable conclave-delete";
      deleteBtn.textContent = "Delete conclave";
      deleteBtn.title = "Delete this conclave and its history from your vault";
      deleteBtn.onclick = () => this.openConclaveDelete(conclave.id);
      actions.appendChild(deleteBtn);
    } else if (isConvener) {
      // The convener is the only authority for inviting and ending.
      const inviteBtn = document.createElement("button");
      inviteBtn.type = "button";
      inviteBtn.className = "btn-secondary pressable conclave-invite";
      inviteBtn.appendChild(icon("addMember"));
      const inviteLabel = document.createElement("span");
      inviteLabel.textContent = "Invite";
      inviteBtn.appendChild(inviteLabel);
      const remaining = candidatesToInvite(
        conclave,
        this.contacts.map((c) => c.nickname),
      );
      inviteBtn.disabled = remaining.length === 0;
      inviteBtn.title = remaining.length
        ? "Invite a trustee"
        : "Everyone you know is already in this conclave";
      inviteBtn.onclick = () => {
        this.conclaveInviteOpen = true;
        this.conclaveInviteTrustees = [];
        this.render();
      };

      const endBtn = document.createElement("button");
      endBtn.type = "button";
      endBtn.className = "btn-secondary pressable conclave-end";
      endBtn.textContent = "End conclave";
      endBtn.title = "End this conclave for everyone";
      endBtn.onclick = () => this.openConclaveEnd(conclave.id);

      actions.append(inviteBtn, endBtn);
    } else {
      // Any joined trustee may leave; the convener leaving ends it instead.
      const leaveBtn = document.createElement("button");
      leaveBtn.type = "button";
      leaveBtn.className = "btn-secondary pressable conclave-leave";
      leaveBtn.textContent = "Leave conclave";
      leaveBtn.title = "Leave this conclave";
      leaveBtn.onclick = () => void this.leaveConclave(conclave.id);
      actions.appendChild(leaveBtn);
    }
    header.append(titles, actions);

    // Trustee chips. Every trustee is shown with their state. Only the convener
    // may kick, and the convener cannot be removed here: leaving as the convener
    // ends the conclave.
    const members = document.createElement("div");
    members.className = "conclave-members";
    for (const trustee of conclave.trustees) {
      const chip = document.createElement("span");
      chip.className = `member-chip member-chip-${trustee.state}`;
      const label = document.createElement("span");
      label.className = "member-chip-label";
      label.textContent =
        trustee.accountId === me ? "You" : displayNick(trustee.nick);
      chip.appendChild(label);

      if (trustee.accountId === conclave.convener) {
        chip.classList.add("member-chip-convener");
        const tag = document.createElement("span");
        tag.className = "member-chip-tag";
        tag.textContent = "convener";
        chip.appendChild(tag);
      } else if (trustee.state === "invited") {
        const tag = document.createElement("span");
        tag.className = "member-chip-tag";
        tag.textContent = "invited";
        chip.appendChild(tag);
      } else if (trustee.state === "left") {
        const tag = document.createElement("span");
        tag.className = "member-chip-tag";
        tag.textContent = "left";
        chip.appendChild(tag);
      } else if (trustee.state === "kicked") {
        const tag = document.createElement("span");
        tag.className = "member-chip-tag";
        tag.textContent = "removed";
        chip.appendChild(tag);
      }

      const canKick =
        !conclave.ended &&
        isConvener &&
        trustee.accountId !== me &&
        (trustee.state === "joined" || trustee.state === "invited");
      if (canKick) {
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "member-chip-remove";
        rm.setAttribute(
          "aria-label",
          `Remove ${displayNick(trustee.nick)} from ${conclave.name}`,
        );
        rm.title = "Remove from conclave";
        rm.textContent = "×";
        rm.onclick = () =>
          void this.kickConclaveTrustee(conclave.id, trustee.nick);
        chip.appendChild(rm);
      }
      members.appendChild(chip);
    }

    const scroll = document.createElement("div");
    scroll.className = "conclave-messages";
    if (!conclave.messages.length) {
      const hint = document.createElement("p");
      hint.className = "muted text-13 conclave-no-messages";
      hint.textContent = "No messages yet. Say hello.";
      scroll.appendChild(hint);
    }
    let lastDay = "";
    for (const msg of conclave.messages) {
      const day = dayLabel(msg.createdAt);
      if (day !== lastDay) {
        lastDay = day;
        const sep = document.createElement("div");
        sep.className = "day-sep";
        sep.textContent = day;
        scroll.appendChild(sep);
      }
      const group = document.createElement("div");
      group.className = `bubble-group ${msg.outgoing ? "out" : "in"}`;
      if (!msg.outgoing) {
        // A conclave is several people, so the sender has to be named.
        const who = document.createElement("div");
        who.className = "conclave-sender text-12 muted";
        who.textContent = msg.from ? displayNick(msg.from) : "Unknown";
        group.appendChild(who);
      }
      const b = document.createElement("div");
      b.className = `bubble ${msg.outgoing ? "out" : "in"} tail`;
      if (msg.file) {
        // A shared file is a file card, not a body: `body` is empty for one.
        const file = msg.file;
        const fc = document.createElement("div");
        fc.className = "file-card";
        const nameEl = document.createElement("div");
        nameEl.className = "text-15";
        nameEl.style.fontWeight = "500";
        nameEl.textContent = file.name;
        const sizeEl = document.createElement("div");
        sizeEl.className = "text-13 muted";
        sizeEl.textContent = formatBytes(file.size);
        fc.append(nameEl, sizeEl);
        attachFileActions(
          fc,
          file.name,
          () => resolveMessageFileBytes(file),
          file.blobId,
        );
        b.appendChild(fc);
      } else {
        b.textContent = msg.body;
      }
      group.appendChild(b);
      scroll.appendChild(group);
    }
    scroll.scrollTop = scroll.scrollHeight;

    // Chat and Paper are the two surfaces of the same meeting; the pill swaps
    // between them without touching the header or the members row. Built before
    // the ended check because an ended conclave keeps both surfaces.
    const tabs = this.renderConclaveTabs();

    // An ended conclave is read-only: the history is shown, but there is no
    // composer. The paper stays reachable — the daemon keeps it readable and
    // refuses only the writes — so the tab strip and the read-only editor are
    // drawn here too, rather than the whole meeting collapsing to one note.
    if (conclave.ended) {
      if (this.conclaveTab === "paper") {
        pane.append(header, members, tabs, this.renderPaperSurface(conclave.id, me));
        return pane;
      }
      const note = document.createElement("div");
      note.className = "conclave-ended-note muted text-13";
      note.textContent =
        "This conclave has ended. Its history stays here, and no one can send in it.";
      pane.append(header, members, tabs, scroll, note);
      return pane;
    }

    // No offline gate here: a conclave message is store-and-forward, so a
    // trustee who is away still gets it when they next sync.
    const composer = document.createElement("div");
    composer.className = "composer";
    const inner = document.createElement("div");
    inner.className = "composer-inner";
    // Sharing a file fans one encrypted copy out to every joined trustee, the
    // same set text goes to. There is no shared history, so it is not backfilled
    // for a trustee who joins later.
    const attach = document.createElement("button");
    attach.type = "button";
    attach.className = "pressable";
    attach.title = "Share a file";
    attach.setAttribute("aria-label", "Share a file");
    attach.appendChild(icon("attach"));
    const fileIn = document.createElement("input");
    fileIn.type = "file";
    fileIn.hidden = true;
    attach.onclick = () => fileIn.click();
    fileIn.onchange = async () => {
      const f = fileIn.files?.[0];
      if (f) await this.sendConclaveFile(conclave.id, f);
      fileIn.value = "";
    };
    const ta = document.createElement("textarea");
    ta.rows = 1;
    ta.placeholder = "Message conclave";
    const send = document.createElement("button");
    send.className = "send-fab btn-icon-round pressable";
    send.appendChild(icon("send"));
    const syncSend = () =>
      send.classList.toggle("ready", ta.value.trim().length > 0);
    ta.oninput = () => {
      syncSend();
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight}px`;
    };
    ta.onkeydown = (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void this.sendConclaveMessage(conclave.id, ta.value.trim(), ta, send);
      }
    };
    send.onclick = () =>
      void this.sendConclaveMessage(conclave.id, ta.value.trim(), ta, send);
    inner.append(attach, fileIn, ta, send);
    composer.appendChild(inner);
    syncSend();

    if (this.conclaveTab === "paper") {
      pane.append(header, members, tabs, this.renderPaperSurface(conclave.id, me));
      return pane;
    }
    pane.append(header, members, tabs, scroll, composer);
    return pane;
  }

  /** Leave the Paper surface: drop the editor, its sync loop and the host.
   *  Called on conclave switch, rail switch, and vault lock. */
  private leavePaperTab(): void {
    closePaper();
    this.paperHost = null;
    this.conclaveTab = "chat";
  }

  /** The Chat | Paper pill for the conclave detail. */
  private renderConclaveTabs(): HTMLElement {
    const tabs = document.createElement("div");
    tabs.className = "segmented conclave-tabs";
    tabs.setAttribute("role", "tablist");
    tabs.setAttribute("aria-label", "Conclave surfaces");
    for (const t of ["chat", "paper"] as const) {
      const b = document.createElement("button");
      b.type = "button";
      b.setAttribute("role", "tab");
      b.textContent = t === "chat" ? "Chat" : "Paper";
      const active = this.conclaveTab === t;
      b.classList.toggle("active", active);
      b.setAttribute("aria-selected", String(active));
      b.onclick = () => {
        if (this.conclaveTab === t) return;
        if (t === "chat") {
          // Leaving the paper flushes its pending edits and stops the timers.
          closePaper();
          this.paperHost = null;
          this.conclaveTab = "chat";
        } else {
          this.conclaveTab = "paper";
        }
        this.render();
      };
      tabs.appendChild(b);
    }
    return tabs;
  }

  /**
   * The Paper surface: the editor host, or the affordances around it.
   *
   * The host element is built once per paper session and re-parented on
   * re-renders; only the loading and start states are rebuilt.
   */
  private renderPaperSurface(conclaveId: string, me: string): HTMLElement {
    const s = paperSession();
    if (!s || s.conclaveId !== conclaveId) {
      // (Re)entering the tab: open the paper. Until it resolves the surface
      // shows a quiet placeholder, then one re-render swaps in the editor.
      const loading = document.createElement("div");
      loading.className = "paper-pane paper-pane-loading muted text-13";
      loading.textContent = "Opening paper…";
      void openPaper(conclaveId)
        .then(() => {
          if (this.conclaveTab === "paper" && this.conclaveId === conclaveId) {
            this.render();
          }
        })
        .catch((e) => {
          if (this.conclaveTab !== "paper" || this.conclaveId !== conclaveId) return;
          loading.textContent =
            e instanceof AppError ? e.message : "Couldn't open the paper.";
        });
      return loading;
    }
    if (this.paperHost) return this.paperHost;
    const host = document.createElement("div");
    host.className = "paper-pane";
    if (!s.exists) {
      // One paper per conclave; the first trustee to start it wins, and the
      // title is just a label everyone sees.
      const start = document.createElement("div");
      start.className = "paper-start";
      const h = document.createElement("h2");
      h.className = "text-17";
      h.textContent = "No paper yet";
      const p = document.createElement("p");
      p.className = "muted text-13";
      p.textContent =
        "Start a shared page of notes for this conclave. Every trustee can edit it, and it is stored encrypted in each vault.";
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "btn-secondary pressable";
      btn.textContent = "Start paper";
      btn.onclick = () => {
        btn.disabled = true;
        void createPaper(conclaveId, `${conclaveById(conclaveId)?.name ?? "Conclave"} paper`)
          .then(() => {
            this.paperHost = null;
            if (this.conclaveTab === "paper") this.render();
          })
          .catch((e) => {
            btn.disabled = false;
            this.announce(
              e instanceof AppError ? e.message : "Couldn't start the paper.",
            );
          });
      };
      start.append(h, p, btn);
      host.appendChild(start);
      this.paperHost = host;
      return host;
    }
    try {
      const conclave = conclaveById(conclaveId);
      // An ended conclave is read-only on every peer, and so is one this vault
      // is no longer a joined trustee of (kicked, or an invitation never
      // accepted): the daemon refuses the write and the relay refuses the
      // fan-out behind it, so the surface must not invite an edit.
      attachEditor(s, host, me, paperReadOnlyReason(conclave, me));
    } catch (e) {
      // render() clears the root before it rebuilds, so a throw here would
      // abort the whole paint and leave a blank window. The editor is the one
      // part of the shell that mounts a third-party view, so it fails into a
      // visible pane the user can leave, not into nothing.
      console.error("paper editor failed to mount", e);
      this.paperHost = null;
      const failed = document.createElement("div");
      failed.className = "paper-pane paper-pane-error muted text-13";
      failed.textContent =
        "The paper editor couldn't open. Switch tabs and try again.";
      return failed;
    }
    this.paperHost = host;
    return host;
  }

  private openConclaveCreate(): void {
    this.conclaveCreateOpen = true;
    this.conclaveCreateName = "";
    this.conclaveCreateTrustees = [];
    this.render();
  }

  private renderConclaveCreateModal(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal conclave-create-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "New conclave");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "New conclave";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-2)";
    p.textContent =
      "Give it a name and choose who to invite. They join once they accept.";

    const nameLabel = document.createElement("label");
    nameLabel.className = "conclave-field";
    const nameText = document.createElement("span");
    nameText.className = "text-13";
    nameText.textContent = "Name";
    const nameInput = document.createElement("input");
    nameInput.type = "text";
    nameInput.className = "input-field";
    nameInput.placeholder = "e.g. Weekly sync";
    nameInput.autocomplete = "off";
    nameInput.value = this.conclaveCreateName;
    nameLabel.append(nameText, nameInput);

    const membersField = document.createElement("div");
    membersField.className = "conclave-field";
    const membersText = document.createElement("span");
    membersText.className = "text-13";
    membersText.textContent = "Trustees";
    membersField.appendChild(membersText);

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.conclaveCreateOpen = false;
      this.render();
    };
    const create = document.createElement("button");
    create.type = "button";
    create.className = "btn-primary pressable";
    create.textContent = "Create conclave";
    // Declared before the picker so its change callback can reach it without a
    // temporal-dead-zone reference.
    const syncCreate = () => {
      create.disabled =
        this.conclaveCreateName.trim().length === 0 ||
        this.conclaveCreateTrustees.length === 0;
    };

    const picker = new ContactPicker({
      items: this.contacts.map((c) => ({
        nickname: c.nickname,
        label: c.mail_handle
          ? `${displayNick(c.nickname)} (${this.contactAddress(c)})`
          : displayNick(c.nickname),
        keywords: c.nickname,
      })),
      multiple: true,
      values: this.conclaveCreateTrustees,
      // Opens above the field so the list cannot cover the Create button below.
      dropUp: true,
      label: "Trustees",
      placeholder: "Type a name…",
      emptyMessage: "No contacts yet. Add someone in Trusts first.",
      noMatchMessage: "No contacts match that name.",
      onChangeMulti: (nicks) => {
        this.conclaveCreateTrustees = nicks;
        syncCreate();
      },
    });
    membersField.appendChild(picker.element);

    nameInput.oninput = () => {
      this.conclaveCreateName = nameInput.value;
      syncCreate();
    };
    create.onclick = () => void this.createConclave();
    syncCreate();

    row.append(cancel, create);
    modal.append(h, p, nameLabel, membersField, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.conclaveCreateOpen = false;
        this.render();
      }
    };
    queueMicrotask(() => nameInput.focus());
    return back;
  }

  private renderConclaveInviteModal(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal conclave-invite-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    const conclave = this.conclaveId ? conclaveById(this.conclaveId) : undefined;
    modal.setAttribute("aria-label", "Invite a trustee");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "Invite a trustee";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-2)";
    p.textContent = conclave
      ? `Invite someone to ${conclave.name}. They join once they accept the invite.`
      : "Invite someone to this conclave.";

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.conclaveInviteOpen = false;
      this.render();
    };
    const invite = document.createElement("button");
    invite.type = "button";
    invite.className = "btn-primary pressable";
    invite.textContent = "Invite";
    const syncInvite = () => {
      invite.disabled = this.conclaveInviteTrustees.length === 0;
    };

    const remaining = conclave
      ? candidatesToInvite(conclave, this.contacts.map((c) => c.nickname))
      : [];
    const picker = new ContactPicker({
      items: remaining.map((nick) => {
        const c = this.contacts.find((x) => x.nickname === nick);
        return {
          nickname: nick,
          label: c?.mail_handle
            ? `${displayNick(nick)} (${this.contactAddress(c)})`
            : displayNick(nick),
          keywords: nick,
        };
      }),
      multiple: true,
      values: this.conclaveInviteTrustees,
      // Opens above the field so the list cannot cover the Invite button below.
      dropUp: true,
      label: "Trustees to invite",
      placeholder: "Type a name…",
      emptyMessage: "Everyone you know is already in this conclave.",
      noMatchMessage: "No contacts match that name.",
      onChangeMulti: (nicks) => {
        this.conclaveInviteTrustees = nicks;
        syncInvite();
      },
    });

    invite.onclick = () => void this.inviteConclaveTrustees();
    syncInvite();

    row.append(cancel, invite);
    modal.append(h, p, picker.element, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.conclaveInviteOpen = false;
        this.render();
      }
    };
    return back;
  }

  private async createConclave(): Promise<void> {
    const name = this.conclaveCreateName.trim();
    if (!name || this.conclaveCreateTrustees.length === 0) {
      this.announce("A conclave needs a name and at least one trustee to invite.");
      return;
    }
    try {
      const created = await api.createConclave(name, this.conclaveCreateTrustees);
      this.conclaveCreateOpen = false;
      await this.reloadConclaves();
      if (created) this.conclaveId = created.id;
      this.announce(`${name} created. Invites sent.`);
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't create the conclave.",
      );
    }
  }

  private async inviteConclaveTrustees(): Promise<void> {
    const conclave = this.conclaveId ? conclaveById(this.conclaveId) : undefined;
    if (!conclave || this.conclaveInviteTrustees.length === 0) return;
    try {
      for (const nick of this.conclaveInviteTrustees) {
        await api.inviteConclaveTrustee(conclave.id, nick);
      }
      this.conclaveInviteOpen = false;
      this.conclaveInviteTrustees = [];
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't send that invite.",
      );
    }
  }

  private async kickConclaveTrustee(
    conclaveId: string,
    nickname: string,
  ): Promise<void> {
    try {
      await api.kickConclaveTrustee(conclaveId, nickname);
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't remove that trustee.",
      );
    }
  }

  private async leaveConclave(conclaveId: string): Promise<void> {
    try {
      await api.leaveConclave(conclaveId);
      if (this.conclaveId === conclaveId) {
        this.leavePaperTab();
        this.conclaveId = null;
      }
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't leave the conclave.",
      );
    }
  }

  private async acceptConclaveInvite(conclaveId: string): Promise<void> {
    try {
      const joined = await api.acceptConclaveInvite(conclaveId);
      await this.reloadConclaves();
      if (joined) {
        this.leavePaperTab();
        this.conclaveId = joined.id;
      }
      this.announce("Invite accepted.");
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't accept the invite.",
      );
    }
  }

  private async declineConclaveInvite(conclaveId: string): Promise<void> {
    try {
      await api.declineConclaveInvite(conclaveId);
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't decline the invite.",
      );
    }
  }

  private async sendConclaveMessage(
    conclaveId: string,
    text: string,
    ta: HTMLTextAreaElement,
    send: HTMLButtonElement,
  ): Promise<void> {
    if (!text) return;
    try {
      await api.sendConclaveMessage(conclaveId, text);
      // Cleared only on success, so a failure leaves the text in the box.
      ta.value = "";
      send.classList.remove("ready");
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't send. Try again.",
      );
    }
  }

  /**
   * Share a file into a conclave.
   *
   * Mirrors the 1:1 file flow: the bytes go to the daemon, which stores a local
   * blob and fans one encrypted copy out per joined trustee. The message is
   * appended to the sender's conclave log by the daemon, so a reload shows it.
   */
  private async sendConclaveFile(conclaveId: string, file: File): Promise<void> {
    if (file.size > MAX_FILE_BYTES) {
      this.announce(
        `Files are limited to ${Math.round(MAX_FILE_BYTES / 1024)} KB for now`,
      );
      return;
    }
    try {
      const b64 = await fileToBase64(file);
      await api.sendConclaveFileData(conclaveId, file.name, b64);
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't share the file. Try again.",
      );
    }
  }

  /** Open the end dialog for a conclave, always with an empty confirmation. */
  private openConclaveEnd(id: string): void {
    this.conclaveEndId = id;
    this.conclaveEndConfirm = "";
    this.render();
  }

  /** Close the end dialog and forget anything typed into it. */
  private closeConclaveEnd(): void {
    this.conclaveEndId = null;
    this.conclaveEndConfirm = "";
  }

  /**
   * The end-conclave dialog.
   *
   * Mirrors the Trust delete dialog: a typed confirmation, focused on the field
   * rather than the destructive button, so ending is never one click. The copy
   * says plainly that this is local — the message log stays in the vault and no
   * other member is notified, because membership is local-only.
   */
  private renderConclaveEndModal(): HTMLElement {
    const id = this.conclaveEndId!;
    const conclave = conclaveById(id);
    const name = conclave ? conclave.name : "this conclave";
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal conclave-end-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", `End ${name}`);

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = `End ${name}?`;
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-3)";
    p.textContent =
      "This ends the conclave for everyone. Its messages stay in every trustee's vault, so the history is kept rather than destroyed.";
    const p2 = document.createElement("p");
    p2.className = "muted text-13";
    p2.style.marginTop = "var(--space-2)";
    p2.textContent =
      "Every joined trustee is told, and the conclave becomes read-only on their side too — no one can send in it after this.";
    const note = document.createElement("p");
    note.className = "faint text-12";
    note.style.marginTop = "var(--space-2)";
    note.textContent =
      "Ending cannot be undone. As the convener you could also leave, which ends it the same way.";

    const label = document.createElement("label");
    label.className = "trust-delete-field";
    const labelText = document.createElement("span");
    labelText.className = "text-13";
    labelText.textContent = "Type END to confirm";
    const input = document.createElement("input");
    input.type = "text";
    input.className = "input-field";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocapitalize", "characters");
    input.setAttribute("spellcheck", "false");
    input.value = this.conclaveEndConfirm;
    label.append(labelText, input);

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.closeConclaveEnd();
      this.render();
    };
    const end = document.createElement("button");
    end.type = "button";
    end.className = "btn-primary pressable conclave-end-confirm";
    end.style.background = "var(--danger)";
    end.textContent = "End conclave";
    // One rule, checked by the button, the Enter key, and the click handler, so
    // there is no way to end a conclave without typing the word.
    const confirmed = () => input.value.trim().toUpperCase() === "END";
    const sync = () => {
      this.conclaveEndConfirm = input.value;
      end.disabled = !confirmed();
    };
    input.oninput = sync;
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (confirmed()) void this.endConclave(id);
      }
    };
    sync();
    end.onclick = () => {
      if (confirmed()) void this.endConclave(id);
    };

    row.append(cancel, end);
    modal.append(h, p, p2, note, label, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.closeConclaveEnd();
        this.render();
      }
    };
    // Focus the field, not the destructive button, so a stray Enter cannot end
    // the conclave.
    queueMicrotask(() => input.focus());
    return back;
  }

  private async endConclave(id: string): Promise<void> {
    try {
      await api.endConclave(id);
      this.closeConclaveEnd();
      await this.reloadConclaves();
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't end the conclave.",
      );
    }
  }

  /** Open the delete dialog for an ended conclave, always with an empty confirmation. */
  private openConclaveDelete(id: string): void {
    this.conclaveDeleteId = id;
    this.conclaveDeleteConfirm = "";
    this.render();
  }

  /** Close the delete dialog and forget anything typed into it. */
  private closeConclaveDelete(): void {
    this.conclaveDeleteId = null;
    this.conclaveDeleteConfirm = "";
  }

  /**
   * The delete-conclave dialog, for an ended conclave.
   *
   * Mirrors the end dialog: a typed confirmation, focused on the field rather
   * than the destructive button, so deleting is never one click. The copy says
   * plainly what goes and what stays — this vault's copy of the conclave and
   * its message log are destroyed, the other trustees keep theirs, and the
   * files shared in the conclave stay in the vault.
   */
  private renderConclaveDeleteModal(): HTMLElement {
    const id = this.conclaveDeleteId!;
    const conclave = conclaveById(id);
    const name = conclave ? conclave.name : "this conclave";
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal conclave-delete-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", `Delete ${name}`);

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = `Delete ${name}?`;
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-3)";
    p.textContent =
      "This deletes the conclave and its entire message history from your vault. Nothing is sent to the other trustees, who keep their own copies.";
    const kept = document.createElement("p");
    kept.className = "muted text-13";
    kept.style.marginTop = "var(--space-2)";
    kept.textContent =
      "The files shared in this conclave stay in your vault — only the messages about them are deleted.";
    const note = document.createElement("p");
    note.className = "faint text-12";
    note.style.marginTop = "var(--space-2)";
    note.textContent = "Deleting cannot be undone.";

    const label = document.createElement("label");
    label.className = "trust-delete-field";
    const labelText = document.createElement("span");
    labelText.className = "text-13";
    labelText.textContent = "Type DELETE to confirm";
    const input = document.createElement("input");
    input.type = "text";
    input.className = "input-field";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocapitalize", "characters");
    input.setAttribute("spellcheck", "false");
    input.value = this.conclaveDeleteConfirm;
    label.append(labelText, input);

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.closeConclaveDelete();
      this.render();
    };
    const del = document.createElement("button");
    del.type = "button";
    del.className = "btn-primary pressable conclave-delete-confirm";
    del.style.background = "var(--danger)";
    del.textContent = "Delete conclave";
    // One rule, checked by the button, the Enter key, and the click handler,
    // so there is no way to delete a conclave without typing the word.
    const confirmed = () => input.value.trim().toUpperCase() === "DELETE";
    const sync = () => {
      this.conclaveDeleteConfirm = input.value;
      del.disabled = !confirmed();
    };
    input.oninput = sync;
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (confirmed()) void this.deleteConclave(id);
      }
    };
    sync();
    del.onclick = () => {
      if (confirmed()) void this.deleteConclave(id);
    };

    row.append(cancel, del);
    modal.append(h, p, kept, note, label, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.closeConclaveDelete();
        this.render();
      }
    };
    // Focus the field, not the destructive button, so a stray Enter cannot
    // delete the conclave.
    queueMicrotask(() => input.focus());
    return back;
  }

  private async deleteConclave(id: string): Promise<void> {
    try {
      await api.deleteConclave(id);
      this.closeConclaveDelete();
      if (this.conclaveId === id) {
        this.leavePaperTab();
        this.conclaveId = null;
      }
      await this.reloadConclaves();
      this.announce("Conclave deleted. Files it shared stay in your vault.");
      this.render();
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't delete the conclave.",
      );
    }
  }

  /** Open the delete dialog for a contact, always with an empty confirmation. */
  private openTrustDelete(nick: string): void {
    this.trustDeleteNick = nick;
    this.trustDeleteConfirm = "";
    this.render();
  }

  /** Close the delete dialog and forget anything typed into it. */
  private closeTrustDelete(): void {
    this.trustDeleteNick = null;
    this.trustDeleteConfirm = "";
  }

  private renderTrustDeleteModal(): HTMLElement {
    const nick = this.trustDeleteNick!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal trust-delete-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", `Delete ${displayNick(nick)} from Trusts`);

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = `Delete ${displayNick(nick)}?`;
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-3)";
    p.textContent = `This removes ${displayNick(nick)} from Trusts. Your chat and mail history with them stays in your vault.`;
    const p2 = document.createElement("p");
    p2.className = "muted text-13";
    p2.style.marginTop = "var(--space-2)";
    p2.textContent =
      "They disappear from Chats, Mail and Trusts, and they can no longer message you. This cannot be undone.";
    // The one honest limit: a contact added before per-contact queues existed is
    // still on the shared inbox, which is not burned (that would cut off every
    // old contact at once). They become revocable the moment they pick up their
    // own private queue.
    const limit = document.createElement("p");
    limit.className = "faint text-12";
    limit.style.marginTop = "var(--space-2)";
    limit.textContent =
      "A contact you added before this update may still be able to send to the old shared inbox until they pick up their own private queue. Nothing they send will open on this device, and they become fully cut off once they do.";
    const note = document.createElement("p");
    note.className = "faint text-12";
    note.style.marginTop = "var(--space-2)";
    note.textContent =
      "You can add them back later with a fresh invite; their old messages stay in your vault.";

    const label = document.createElement("label");
    label.className = "trust-delete-field";
    const labelText = document.createElement("span");
    labelText.className = "text-13";
    labelText.textContent = "Type DELETE to confirm";
    const input = document.createElement("input");
    input.type = "text";
    input.className = "input-field";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocapitalize", "characters");
    input.setAttribute("spellcheck", "false");
    input.value = this.trustDeleteConfirm;
    label.append(labelText, input);

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.closeTrustDelete();
      this.render();
    };
    const del = document.createElement("button");
    del.type = "button";
    del.className = "btn-primary pressable trust-delete-confirm";
    del.style.background = "var(--danger)";
    del.textContent = "Delete";
    // Matched case-insensitively on trimmed text, and gated on the same check
    // for the button, the Enter key, and the click handler, so there is one
    // rule and no way around it.
    const confirmed = () => input.value.trim().toUpperCase() === "DELETE";
    const sync = () => {
      this.trustDeleteConfirm = input.value;
      del.disabled = !confirmed();
    };
    input.oninput = sync;
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (confirmed()) void this.deleteTrust(nick);
      }
    };
    sync();
    del.onclick = () => {
      if (confirmed()) void this.deleteTrust(nick);
    };

    row.append(cancel, del);
    modal.append(h, p, p2, limit, note, label, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.closeTrustDelete();
        this.render();
      }
    };
    // Focus the field, not the destructive button, so a stray Enter cannot
    // delete anyone.
    queueMicrotask(() => input.focus());
    return back;
  }

  /**
   * Remove a contact and revoke their ability to message this account.
   *
   * History is deliberately not touched: the chat log and mail records stay in
   * the vault, and the contact simply stops being surfaced. What changed from a
   * plain removal is that the daemon now calls `revoke_contact`, which burns the
   * per-contact queue this account minted for them. The deleted person then has
   * no queue to deliver into, and anything already queued is dropped rather than
   * filed.
   *
   * The one limit: a contact added before per-contact queues existed is still on
   * the shared inbox, which is not burned because that would cut off every old
   * contact at once. Such a contact is not revocable until they receive their own
   * queue; the delete modal says so.
   */
  private async deleteTrust(nick: string): Promise<void> {
    try {
      await api.removeContact(nick);
      await clearContactUiPrefs(nick);
      this.contacts = (await api.contacts()) ?? [];
      setKnownContacts(this.contacts.map((c) => c.nickname));
      if (this.selectedNick === nick) this.selectedNick = null;
      if (this.filesContactNick === nick) {
        this.filesContactNick = null;
        this.filesOpen = false;
      }
      if (this.profileNick === nick) this.profileNick = null;
      // The open conversation and composer belong to the person being removed,
      // so neither may keep showing them.
      this.mailThreadId = null;
      this.mailOpenId = null;
      if (this.mailComposeTo === nick) {
        this.mailComposeOpen = false;
        this.mailDraftId = null;
        this.mailComposeTo = "";
        this.mailComposeSubject = "";
        this.mailComposeBody = "";
        this.mailComposeFiles = [];
      }
      this.closeTrustDelete();
      await this.reloadMailbox();
      this.announce(`${nick} removed from your circle`);
      this.render();
    } catch (e) {
      this.closeTrustDelete();
      this.announce(e instanceof AppError ? e.message : "Could not remove contact");
      this.render();
    }
  }

  // --- Exchanged files (browser over chat and mail attachments) -----------
  //
  // Reached from the Vault bento tile. Like Notes and Wallet, it renders as a
  // full-bleed sub-pane inside the Vault rather than a rail of its own: the
  // bytes are sealed in the vault, so the vault is where you browse them.
  // A file counts as exchanged whether it arrived over a 1:1 chat or as a
  // mail attachment; both stores are read and merged per contact.

  /** One row in the Exchanged-files browser: a chat attachment or a mail one. */
  private exchangedRowsFor(
    nick: string,
    direction: "received" | "sent",
  ): ExchangedFileRow[] {
    const outgoing = direction === "sent";
    const chat = fileMessagesForContact(nick, direction).map(
      (m): ExchangedFileRow => ({
        name: m.fileName ?? "file",
        size: m.fileSize ?? 0,
        at: m.createdAt,
        via: "chat",
        bytes: m.fileData || m.blobId ? m : null,
        blobId: m.blobId ?? null,
      }),
    );
    const mail = this.exchangedMailMessages(nick)
      .filter((m) => m.outgoing === outgoing)
      .flatMap((m) =>
        m.attachments.map((a) => ({
          name: a.name,
          size: a.size,
          at: m.createdAt,
          via: "mail" as const,
          bytes: a,
          blobId: a.blobId,
          subject: m.subject,
          mail: m,
        })),
      );
    return [...chat, ...mail].sort((a, b) => b.at - a.at);
  }

  /**
   * Mail with a contact whose attachments count as exchanged files: inbox and
   * sent, plus anything archived out of them. Queued (outbox) mail has not
   * been exchanged yet, trashed mail was disowned, and Drops have no contact.
   */
  private exchangedMailMessages(nick: string): MailMessage[] {
    const flags = this.mailPrefs();
    const seen = new Set<string>();
    const out: MailMessage[] = [];
    for (const m of [
      ...listFolder("inbox", flags),
      ...listFolder("sent", flags),
      ...listFolder("archive", flags),
    ]) {
      if (m.contactNick !== nick || seen.has(m.id)) continue;
      seen.add(m.id);
      out.push(m);
    }
    return out;
  }

  private mailAttachmentCount(nick: string): number {
    return this.exchangedMailMessages(nick).reduce(
      (n, m) => n + m.attachments.length,
      0,
    );
  }

  /** Open the browser from the Vault tile: full-bleed, contact list first. */
  private filesOpenFiles(): void {
    this.filesOpen = true;
    this.walletOpen = false;
    this.notesOpen = false;
    this.notesEditorOpen = false;
    this.notesEditingId = null;
    this.filesContactNick = null;
    this.render();
  }

  private filesBackToVault(): void {
    this.filesOpen = false;
    this.filesContactNick = null;
    this.render();
  }

  private renderFiles(): HTMLElement {
    if (this.filesContactNick) {
      return this.renderFilesForContact(this.filesContactNick);
    }
    return this.renderFilesContactList();
  }

  private renderFilesContactList(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.padding = "var(--space-5)";

    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary text-13 pressable";
    back.textContent = "← Vault";
    back.onclick = () => this.filesBackToVault();

    const h = document.createElement("h1");
    h.className = "text-22";
    h.style.marginTop = "var(--space-4)";
    h.textContent = "Files";
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.style.marginTop = "var(--space-2)";
    hint.textContent =
      "Choose a contact to see files you’ve exchanged — stored only on your devices.";
    const list = document.createElement("div");
    list.style.marginTop = "var(--space-5)";
    list.style.display = "flex";
    list.style.flexDirection = "column";
    list.style.gap = "var(--space-2)";

    const nicks = contactsWithFiles(
      this.contacts.map((c) => c.nickname),
    );
    const also = this.contacts.filter((c) => !nicks.includes(c.nickname));

    for (const nick of [...nicks, ...also.map((c) => c.nickname)]) {
      const c = this.contacts.find((x) => x.nickname === nick);
      if (!c) continue;
      const count = fileCountForContact(nick) + this.mailAttachmentCount(nick);
      const row = document.createElement("button");
      row.type = "button";
      row.className = "conv-row pressable";
      row.style.width = "100%";
      row.style.textAlign = "left";
      const left = document.createElement("div");
      left.style.display = "flex";
      left.style.gap = "var(--space-3)";
      left.style.alignItems = "center";
      left.appendChild(createAvatarEl(c.protocol_name, "sm", false, displayNick(nick)));
      const meta = document.createElement("div");
      const title = document.createElement("div");
      title.className = "text-15";
      title.style.fontWeight = "500";
      title.textContent = displayNick(nick);
      const sub = document.createElement("div");
      sub.className = "text-13 muted";
      sub.textContent =
        count > 0
          ? `${count} file${count === 1 ? "" : "s"} exchanged`
          : "No files yet — open to send";
      meta.append(title, sub);
      left.append(meta);
      row.append(left);
      row.onclick = () => {
        this.filesContactNick = nick;
        this.filesDirection = "received";
        this.render();
      };
      list.appendChild(row);
    }

    if (!this.contacts.length) {
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Add a contact in Chats to exchange files.";
      list.appendChild(p);
    }

    pane.append(back, h, hint, list);
    return pane;
  }

  private renderFilesForContact(nick: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.padding = "var(--space-5)";

    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary text-13 pressable";
    back.textContent = "← All contacts";
    back.onclick = () => {
      this.filesContactNick = null;
      this.render();
    };

    const h = document.createElement("h1");
    h.className = "text-22";
    h.style.marginTop = "var(--space-4)";
    h.textContent = displayNick(nick);
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.textContent = `Files you’ve exchanged with ${displayNick(nick)} — stored only on your devices.`;

    const tabs = document.createElement("div");
    tabs.className = "segmented";
    tabs.style.marginTop = "var(--space-4)";
    tabs.style.maxWidth = "280px";
    const recvTab = document.createElement("button");
    recvTab.type = "button";
    recvTab.className = this.filesDirection === "received" ? "active" : "";
    recvTab.textContent = "Received";
    recvTab.onclick = () => {
      this.filesDirection = "received";
      this.render();
    };
    const sentTab = document.createElement("button");
    sentTab.type = "button";
    sentTab.className = this.filesDirection === "sent" ? "active" : "";
    sentTab.textContent = "Sent";
    sentTab.onclick = () => {
      this.filesDirection = "sent";
      this.render();
    };
    tabs.append(recvTab, sentTab);

    const sendRow = document.createElement("div");
    sendRow.style.marginTop = "var(--space-4)";
    const sendBtn = document.createElement("button");
    sendBtn.type = "button";
    sendBtn.className = "btn-primary pressable";
    sendBtn.textContent = "Send file";
    const fileIn = document.createElement("input");
    fileIn.type = "file";
    fileIn.hidden = true;
    sendBtn.onclick = () => fileIn.click();
    fileIn.onchange = async () => {
      const f = fileIn.files?.[0];
      fileIn.value = "";
      if (f) await this.sendFile(nick, f);
    };
    sendRow.append(sendBtn, fileIn);

    const list = document.createElement("div");
    list.style.marginTop = "var(--space-4)";
    list.style.display = "flex";
    list.style.flexDirection = "column";
    list.style.gap = "var(--space-3)";

    const files = this.exchangedRowsFor(nick, this.filesDirection);
    for (const f of files) {
      const card = document.createElement("div");
      card.className = "tile";
      const nameEl = document.createElement("div");
      nameEl.className = "text-15";
      nameEl.style.fontWeight = "500";
      nameEl.textContent = f.name;
      const sub = document.createElement("div");
      sub.className = "text-13 muted";
      sub.textContent =
        `${formatBytes(f.size)} · ${formatRelative(f.at)} · via ${f.via === "mail" ? "Mail" : "chat"}` +
        (f.subject ? ` — ${f.subject}` : "");
      card.append(nameEl, sub);
      const bytes = f.bytes;
      if (bytes) {
        attachFileActions(card, f.name, () => resolveMessageFileBytes(bytes), f.blobId);
      }
      const link = document.createElement("button");
      link.type = "button";
      link.className = "text-13 pressable";
      link.style.marginTop = "var(--space-2)";
      link.style.color = "var(--accent)";
      const mail = f.mail;
      if (mail) {
        link.textContent = "View in mail";
        link.onclick = () => {
          this.rail = "mail";
          this.filesOpen = false;
          this.filesContactNick = null;
          this.openMessage(mail);
        };
      } else {
        link.textContent = "View in chat";
        link.onclick = () => {
          this.rail = "chats";
          this.selectedNick = nick;
          this.filesOpen = false;
          this.filesContactNick = null;
          this.render();
        };
      }
      card.appendChild(link);
      list.appendChild(card);
    }

    if (!files.length) {
      const p = document.createElement("p");
      p.className = "muted text-13";
      p.textContent =
        this.filesDirection === "received"
          ? "Nothing received yet."
          : "You haven’t sent a file here yet.";
      list.appendChild(p);
    }

    pane.append(back, h, hint, tabs, sendRow, list);
    return pane;
  }

  private mailAddress(): string | null {
    return this.mailHandle ? `${this.mailHandle}@${this.vaultName}` : null;
  }

  /**
   * A contact's mail address: their handle at their own vault, learned from
   * their invite. A contact whose invite predates vault naming falls back to
   * the default vault name rather than this vault's name, which would be a lie
   * about who the address belongs to.
   */
  private contactAddress(c: { mail_handle?: string | null; mail_vault?: string | null }): string {
    return `${c.mail_handle ?? "?"}@${c.mail_vault ?? DEFAULT_VAULT_NAME}`;
  }

  private contactByNick(nick: string): ContactDto | undefined {
    return this.contacts.find((c) => c.nickname === nick);
  }

  /** A 14px icon, for the inline marks in list rows. */
  private iconSmall(name: IconName): SVGElement {
    const el = icon(name);
    el.setAttribute("width", "14");
    el.setAttribute("height", "14");
    return el;
  }

  // ------------------------------------------------------------- sidebar

  // --- Reads (Hacker News reader pane) -------------------------------------

  /** Load the current feed or search page into the list fields, then repaint. */
  private async readsLoad(page = 0): Promise<void> {
    this.readsLoading = true;
    this.readsError = null;
    if (page === 0) {
      this.readsStories = [];
      this.readsPage = 0;
    }
    this.render();
    try {
      const resp = await loadReadsFeed(this.readsSource, page);
      this.readsStories = page === 0 ? resp.stories : [...this.readsStories, ...resp.stories];
      this.readsDone = resp.done;
      this.readsPage = page;
    } catch (e) {
      this.readsError = e instanceof Error ? e.message : String(e);
    } finally {
      this.readsLoading = false;
      this.render();
    }
  }

  /** Browse a different source: the built-in or one of the user's feeds. */
  private readsOpenSource(id: string): void {
    this.readsSource = id;
    this.readsMode = "feed";
    this.readsSourceTitle =
      this.readsSources.find((s) => s.id === id)?.title ?? HK_NEWS.title;
    this.readsOpenId = null;
    this.readsDetail = null;
    this.readsReaderHtml = null;
    void this.readsLoad(0);
  }

  /**
   * Register an RSS feed from the add dialog. The daemon fetches it once before
   * accepting, so a bad URL or an unreadable feed is refused with its reason;
   * a good one is opened right away and the dialog closes.
   *
   * A URL typed without a scheme (`example.com/feed`) is the common case, and
   * the daemon refuses it verbatim — so assume `https://` rather than bouncing
   * the user back with a reason they did not cause.
   */
  private async readsAddFeed(): Promise<void> {
    if (this.readsSourceBusy) return;
    const typed = this.readsSourceDraft.trim();
    if (!typed) {
      this.readsAddError = "Paste an RSS feed URL to add.";
      this.render();
      return;
    }
    const url = /^[a-z][a-z0-9+.-]*:\/\//i.test(typed) ? typed : `https://${typed}`;
    this.readsSourceBusy = true;
    this.readsAddError = null;
    this.render();
    try {
      this.readsSources = await addReadsSource(url);
      this.readsSourceDraft = "";
      this.readsSourceBusy = false;
      this.readsAddOpen = false;
      const added = this.readsSources.find((s) => !s.built_in && s.url === url);
      if (added) this.readsOpenSource(added.id);
      else this.render();
    } catch (e) {
      this.readsSourceBusy = false;
      this.readsAddError = e instanceof Error ? e.message : String(e);
      this.render();
    }
  }

  private async readsRemoveFeed(id: string): Promise<void> {
    try {
      this.readsSources = await removeReadsSource(id);
      if (this.readsSource === id) this.readsOpenSource(HK_NEWS.id);
      else this.render();
    } catch (e) {
      this.readsError = e instanceof Error ? e.message : String(e);
      this.render();
    }
  }

  private readsOpenSaved(): void {
    this.readsMode = "saved";
    this.readsOpenId = null;
    this.readsDetail = null;
    this.readsReaderHtml = null;
    void this.readsLoadSaved();
  }

  /** Saved stories come from personal state, not the feed cache. */
  private async readsLoadSaved(): Promise<void> {
    this.readsLoading = true;
    this.readsError = null;
    this.render();
    try {
      const state = await getReadsState();
      this.readsStories = state.saved.map((x) => ({
        ...x.story,
        saved: true,
        read: state.read.some((m) => m.id === x.story.id),
      }));
      this.readsDone = true;
    } catch (e) {
      this.readsError = e instanceof Error ? e.message : String(e);
    } finally {
      this.readsLoading = false;
      this.render();
    }
  }

  /**
   * Open a story in the reading pane. HN stories fetch the discussion in the
   * background (reader text loads alongside); RSS items have no discussion
   * and go straight to the reader.
   */
  private readsOpenStory(id: string): void {
    const listed = this.readsStories.find((s) => s.id === id);
    this.readsOpenId = id;
    this.readsDetail = null;
    this.readsReaderHtml = null;
    this.readsReaderError = null;
    const isHn = Boolean(listed?.hn_url);
    this.readsView = isHn && !listed?.url ? "comments" : "reader";
    this.readsDetailLoading = isHn;
    this.render();
    if (isHn) {
      void loadReadsStory(id)
        .then((detail) => {
          this.readsDetail = detail;
          if (this.readsView === "reader" && detail.story.url) {
            this.readsLoadReader(detail.story.url);
          }
        })
        .catch((e) => {
          this.readsError = e instanceof Error ? e.message : String(e);
        })
        .finally(() => {
          this.readsDetailLoading = false;
          this.render();
        });
    } else if (listed?.url) {
      this.readsLoadReader(listed.url);
    }
    // Opened is read: fire and forget, then refresh the row's marker.
    if (listed) {
      const title = listed.title;
      void markReadsRead(id, title)
        .then(() => {
          listed.read = true;
          if (this.rail === "reads") this.render();
        })
        .catch(() => {
          /* read markers are best-effort */
        });
    }
  }

  private readsLoadReader(url: string): void {
    this.readsView = "reader";
    if (this.readsReaderHtml !== null) {
      this.render();
      return;
    }
    this.readsReaderLoading = true;
    this.readsReaderError = null;
    this.render();
    void loadReadsReader(url)
      .then((r) => {
        this.readsReaderHtml = r.html;
      })
      .catch((e) => {
        this.readsReaderError = e instanceof Error ? e.message : String(e);
      })
      .finally(() => {
        this.readsReaderLoading = false;
        this.render();
      });
  }

  private readsSwitchView(view: "reader" | "comments"): void {
    const detail = this.readsDetail;
    if (!detail) return;
    if (view === "reader") {
      if (!detail.story.url) return;
      this.readsLoadReader(detail.story.url);
      return;
    }
    this.readsView = "comments";
    this.render();
  }

  private async readsSave(id: string): Promise<void> {
    const listed = this.readsStories.find((s) => s.id === id);
    const detailStory = this.readsDetail?.story;
    const story = listed ?? detailStory;
    if (!story) return;
    try {
      const state = await toggleReadsSaved(story);
      if (this.readsMode === "saved") {
        // Unstarring from the Saved list removes the row.
        await this.readsLoadSaved();
      } else {
        this.readsApplyState(state);
      }
    } catch (e) {
      this.readsError = e instanceof Error ? e.message : String(e);
      this.render();
    }
  }

  private async readsHide(id: string): Promise<void> {
    try {
      const state = await toggleReadsHidden(id);
      this.readsApplyState(state);
      // A hidden story leaves the list, and closes if it was open.
      this.readsStories = this.readsStories.filter((s) => s.id !== id);
      if (this.readsOpenId === id) {
        this.readsOpenId = null;
        this.readsDetail = null;
        this.readsReaderHtml = null;
      }
    } catch (e) {
      this.readsError = e instanceof Error ? e.message : String(e);
    }
    this.render();
  }

  /** Re-annotate list rows from fresh personal state. */
  private readsApplyState(state: ReadsStateDto): void {
    for (const s of this.readsStories) {
      s.saved = state.saved.some((x) => x.story.id === s.id);
      s.read = state.read.some((m) => m.id === s.id);
    }
    this.render();
  }

  /** Every external link routes through the confirm modal; never auto-open. */
  private readsConfirmOpen(url: string): void {
    this.readsConfirmUrl = url;
    this.render();
  }

  private readsHostOf(url: string): string {
    try {
      return new URL(url).host;
    } catch {
      return url;
    }
  }

  // --- Zines (a member's own publication, and the ones they hold) ------------
  //
  // Phase 1 is local and offline: this vault authors one zine under its pen
  // name and reads whatever else it holds. There is no bulletin, no key
  // epidemic, and no comment arriving from a peer yet, so every route here
  // talks to the local daemon and nothing else. The newspaper model is the
  // shape: a pull-only corpus, filtered by section, newest first.

  /**
   * Load the pane.
   *
   * The whole index, the section facet, and the pen name come back in one
   * response — three round trips to paint one screen would be three chances to
   * render a pane that disagrees with itself. The index is fetched unfiltered
   * and the section is applied on screen, so the facet and the list are always
   * read from the same payload.
   */
  private async zinesLoad(): Promise<void> {
    this.zinesLoading = true;
    this.zinesError = null;
    if (this.rail === "zines") this.render();
    try {
      const view = await api.zinesIndex();
      this.zinesView = view ?? null;
    } catch (e) {
      this.zinesError =
        e instanceof AppError ? e.message : "Couldn't load your zines.";
    }
    this.zinesLoading = false;
    if (this.rail === "zines") this.render();
  }

  /**
   * The corpus after the pane's view controls and the byline filter, before the
   * section filter.
   *
   * This is what the section strip counts and what the page is built from, so
   * the two can never disagree: a control that hides stories hides them from
   * the strip as well.
   */
  private zinesHeld(): ZineSummaryDto[] {
    let held = this.zinesView?.zines ?? [];
    if (this.zinesMineOnly) held = this.zinesMine();
    if (!this.zinesShowMuted) held = held.filter((z) => !z.muted);
    return byByline(held, this.zinesPen);
  }

  /** The front page's stories: the held corpus, narrowed by the active section. */
  private zinesFiltered(): ZineSummaryDto[] {
    const held = this.zinesHeld();
    const section = this.zinesSection;
    return section ? held.filter((z) => z.sections.includes(section)) : held;
  }

  /** The section strip, in the order the page files its stories under it. */
  private zinesSections(): ZinesSectionCount[] {
    return sectionCounts(this.zinesHeld());
  }

  /**
   * Flip a front-page view control.
   *
   * A control that empties the section the page is filtered to would leave the
   * reader on a blank page with no way to tell why, so the filter falls back to
   * the whole corpus when the section stops being one the strip lists.
   */
  private zinesSetView(mineOnly: boolean, showMuted: boolean): void {
    this.zinesMineOnly = mineOnly;
    this.zinesShowMuted = showMuted;
    const section = this.zinesSection;
    if (section && !this.zinesSections().some((s) => s.name === section)) {
      this.zinesSection = null;
    }
    this.pendingZinesScroll = null;
    this.render();
  }

  /**
   * Narrow the page to one byline, or clear it.
   *
   * Reached from a byline on a story or a card, which is the only place a reader
   * can learn a byline they did not already know. It clears the section filter
   * rather than stacking with it: the two are different narrowings of the same
   * page, and a page silently filtered twice is a page that looks empty for no
   * visible reason.
   */
  private zinesFilterByByline(pen: string | null): void {
    this.zinesPen = pen;
    if (pen) this.zinesSection = null;
    this.zinesOpenId = null;
    this.zinesDetail = null;
    this.zinesDetailError = null;
    this.pendingZinesScroll = null;
    this.render();
  }

  /** Every post this vault's pen name wrote, newest first. */
  private zinesMine(): ZineSummaryDto[] {
    return (this.zinesView?.zines ?? []).filter((z) => z.authored);
  }

  /** Open a story on its own page. */
  private async zinesOpen(zineId: string): Promise<void> {
    if (this.zinesEditing) this.zinesDiscardDraft();
    // Opening a story leaves the front page, so remember where it was: the
    // reader coming back should land where they were reading, not at the top.
    const front = this.root.querySelector<HTMLElement>(".zines-page");
    if (front && !this.zinesOpenId) this.pendingZinesScroll = front.scrollTop;
    this.zinesOpenId = zineId;
    this.zinesDetail = null;
    this.zinesDetailError = null;
    // A comment belongs to the story it was typed on, so opening another story
    // starts a fresh box rather than carrying the text across.
    this.zinesCommentHost = null;
    this.zinesCommentDraft = "";
    this.zinesCommentError = null;
    this.zinesCommentBusyHash = null;
    this.render();
    try {
      const detail = await api.zinesGet(zineId);
      // The selection can move while the request is in flight; a late reply
      // must not paint over whatever the user is looking at now.
      if (this.zinesOpenId !== zineId) return;
      this.zinesDetail = detail;
    } catch (e) {
      if (this.zinesOpenId !== zineId) return;
      this.zinesDetailError =
        e instanceof AppError ? e.message : "Couldn't open that zine.";
    }
    if (this.rail === "zines" && this.zinesOpenId === zineId) this.render();
  }

  /**
   * A keystroke in the draft.
   *
   * Deliberately not a re-render: the editor is a third-party view and
   * re-parenting it mid-keystroke is how a caret ends up in the wrong
   * paragraph. The one thing that has to change is Publish, which stays
   * disabled until there is something to publish.
   */
  private zinesDraftChanged(): void {
    const btn = this.zinesPublishBtn;
    if (btn && btn.textContent !== "Publishing…") btn.disabled = false;
  }

  /**
   * Open the draft editor for a new post, or a new revision of one.
   *
   * `zineId` is the post being revised, or null to publish a new one: the daemon
   * takes the byline from the vault and mints the post's id, because a pen name
   * holds many posts and an id is an address rather than an identity.
   */
  private zinesStartDraft(
    zineId: string | null,
    title: string,
    sections: string,
    seedHtml: string,
  ): void {
    closeZineEditor();
    this.zinesEditorHost = null;
    this.zinesEditing = true;
    this.zinesDraftZineId = zineId;
    this.zinesDraftTitle = title;
    this.zinesDraftSections = sections;
    this.zinesDraftSeedHtml = seedHtml;
    this.zinesPublishBtn = null;
    this.render();
  }

  /**
   * Drop the draft and its editor.
   *
   * A draft lives only in the editor's document — Phase 1 has nowhere to keep
   * an unpublished revision — so discarding it is the honest end state rather
   * than leaving it on screen looking publishable after the vault locked or the
   * rail moved on.
   */
  private zinesDiscardDraft(): void {
    closeZineEditor();
    this.zinesEditorHost = null;
    this.zinesEditing = false;
    this.zinesDraftZineId = null;
    this.zinesDraftTitle = "";
    this.zinesDraftSections = "";
    this.zinesDraftSeedHtml = "";
    this.zinesPublishBtn = null;
  }

  /** Revise the open zine, seeded with what it currently holds. */
  private zinesRevise(zineId: string, title: string, sections: string[], body: string): void {
    this.zinesStartDraft(zineId, title, sections.join(", "), body);
  }

  /**
   * Start a new post from the rail's write button.
   *
   * Always a new post, never a revision: the button publishes something new, and
   * revising is what the open article's own "Write a new revision" does, seeded
   * with that article. There is no body to carry over and nothing to overwrite,
   * so an empty editor is the honest state — the earlier shape, where this
   * button had to fetch the pen name's one zine back to avoid blanking it, went
   * away with the one-post-per-pen-name model.
   */
  private zinesStartNewPost(): void {
    this.zinesStartDraft(null, "", "", "");
  }

  /**
   * Publish the draft as a new post, or as a new revision of one.
   *
   * The editor's document is read *before* the button is disabled, because
   * publishing tears the editor down on success and the doc goes with it.
   */
  private async zinesPublish(btn: HTMLButtonElement): Promise<void> {
    const title = this.zinesDraftTitle.trim();
    if (!title) {
      this.announce("A zine needs a title.");
      this.zinesDraftTitleError();
      return;
    }
    const body = zineEditorHtml();
    const sections = parseSections(this.zinesDraftSections);
    const revising = this.zinesDraftZineId;
    btn.disabled = true;
    btn.textContent = "Publishing…";
    try {
      const view = await api.zinesWrite({
        zine_id: revising ?? undefined,
        title,
        sections,
        body,
      });
      if (view) this.zinesView = view;
      // A create mints the post's id, so the answer is where the id to read back
      // comes from; a revision already knew its own.
      const publishedId = view?.zine_id ?? revising ?? null;
      this.zinesDiscardDraft();
      if (publishedId) {
        void this.zinesOpen(publishedId);
      } else if (this.rail === "zines") {
        this.render();
      }
    } catch (e) {
      // The draft survives a refusal: the daemon validates the body it is
      // given, and a rejected publish must not cost the author their writing.
      btn.disabled = false;
      btn.textContent = "Publish";
      this.announce(
        e instanceof AppError ? e.message : "Couldn't publish that zine.",
      );
    }
  }

  /** Mark the title field, so a refused publish points at the field at fault. */
  private zinesDraftTitleError(): void {
    const input = this.root.querySelector<HTMLInputElement>(".zine-title-input");
    if (input) {
      input.classList.add("invalid");
      input.focus();
    }
  }

  private async zinesToggleMute(zineId: string, muted: boolean): Promise<void> {
    try {
      const view = await api.zinesMute(zineId, muted);
      if (view) this.zinesView = view;
      // The reading pane renders from the detail, which was fetched before the
      // toggle, so it is patched from the daemon's answer. Re-fetching would
      // work too, and would flicker the article for a flag with no wire effect.
      const open = this.zinesDetail?.zine;
      if (open && open.descriptor.zine_id === zineId) {
        open.muted = view?.zines.find((z) => z.zine_id === zineId)?.muted ?? muted;
      }
    } catch (e) {
      this.announce(e instanceof AppError ? e.message : "Couldn't change that.");
    }
    if (this.rail === "zines") this.render();
  }

  private async zinesForget(zineId: string): Promise<void> {
    this.zinesForgetId = null;
    try {
      const view = await api.zinesForget(zineId);
      if (view) this.zinesView = view;
      if (this.zinesOpenId === zineId) {
        this.zinesOpenId = null;
        this.zinesDetail = null;
        this.zinesDetailError = null;
      }
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't forget that zine.",
      );
    }
    if (this.rail === "zines") this.render();
  }

  /**
   * Hand the open post's read key to one contact.
   *
   * The dialog's own state is cleared first, so a failure reports into the
   * announcement line rather than leaving a modal open over a story whose
   * reading position the reader has already moved past.
   */
  private async zinesShare(): Promise<void> {
    const zineId = this.zinesShareId;
    const contact = this.zinesShareContact;
    if (!zineId) return;
    if (!contact) {
      this.zinesShareError = "Choose a contact to send it to.";
      this.render();
      return;
    }
    this.zinesShareBusy = true;
    this.zinesShareError = null;
    this.render();
    try {
      const view = await api.zinesGrant(zineId, contact);
      if (view) this.zinesView = view;
      this.zinesShareId = null;
      this.zinesShareContact = "";
      this.announce(
        `Key sent to ${displayNick(contact)}. They will read this post on their next poll.`,
      );
    } catch (e) {
      this.zinesShareError =
        e instanceof AppError ? e.message : "Couldn't send that key.";
    }
    this.zinesShareBusy = false;
    if (this.rail === "zines") this.render();
  }

  /**
   * Publish a comment on the open post.
   *
   * The comment is sealed to the author's pen name and goes to the lane, not to
   * the author's inbox, so this needs no session with them — and the answer's
   * `published` flag is reported honestly rather than assumed, because a relay
   * that refused the put is a comment that only exists in this vault.
   */
  private async zinesPostComment(): Promise<void> {
    const zineId = this.zinesOpenId;
    if (!zineId || this.zinesCommentBusy) return;
    const text = this.zinesCommentDraft.trim();
    if (!text) {
      this.zinesCommentError = "An empty comment is not worth publishing.";
      this.render();
      return;
    }
    this.zinesCommentBusy = true;
    this.zinesCommentError = null;
    this.render();
    try {
      const view = await api.zinesComment(zineId, text);
      if (view) this.zinesView = view;
      // The box is a stable host that outlives this render, so clearing the
      // draft is not enough: the textarea still holds the text that was just
      // published, and leaving it there invites a double post.
      this.zinesCommentDraft = "";
      if (this.zinesCommentHost) this.zinesCommentHost.value = "";
      this.announce(
        view?.published === false
          ? "Comment kept in your vault. The relay refused it just now; your vault will re-post it."
          : "Comment published to the lane. The author sees it once they poll.",
      );
    } catch (e) {
      this.zinesCommentError =
        e instanceof AppError ? e.message : "Couldn't publish that comment.";
    }
    this.zinesCommentBusy = false;
    if (this.rail === "zines") this.render();
  }

  /**
   * Merge one pulled comment, then republish the post.
   *
   * Both halves matter and both are reported: a merge that did not republish is
   * a comment only this vault can see, and the re-post sweep will try again.
   */
  private async zinesAcceptComment(hash: string): Promise<void> {
    const zineId = this.zinesOpenId;
    if (!zineId || this.zinesCommentBusyHash) return;
    this.zinesCommentBusyHash = hash;
    this.render();
    try {
      const view = await api.zinesAcceptComment(zineId, hash);
      if (view) this.zinesView = view;
      this.announce(
        view?.published === false
          ? "Merged into revision " +
              (view?.revision ?? 0) +
              ", but the republish failed: " +
              (view?.publish_error || "the relay refused it") +
              ". Your vault will send it again."
          : `Merged into revision ${view?.revision ?? 0} and republished.`,
      );
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't merge that comment.",
      );
    }
    this.zinesCommentBusyHash = null;
    await this.zinesRefreshDetail(zineId);
  }

  /** Drop a pulled comment from the pile. Not a retraction: it was never out. */
  private async zinesDiscardComment(hash: string): Promise<void> {
    const zineId = this.zinesOpenId;
    if (!zineId || this.zinesCommentBusyHash) return;
    this.zinesCommentBusyHash = hash;
    this.render();
    try {
      const detail = await api.zinesDiscardComment(zineId, hash);
      // The pile is inside the zine, so the detail is patched from the answer
      // rather than re-fetched: this changes nothing about the article.
      if (detail && this.zinesOpenId === zineId) this.zinesDetail = detail;
      this.announce("Comment dropped. It was never merged, so nothing is retracted.");
    } catch (e) {
      this.announce(
        e instanceof AppError ? e.message : "Couldn't drop that comment.",
      );
    }
    this.zinesCommentBusyHash = null;
    if (this.rail === "zines") this.render();
  }

  /**
   * Re-read the open story.
   *
   * A merge changes the descriptor — a new revision, a longer thread — and the
   * detail on screen was fetched before it, so it is refetched rather than
   * patched field by field.
   */
  private async zinesRefreshDetail(zineId: string): Promise<void> {
    try {
      const detail = await api.zinesGet(zineId);
      if (this.zinesOpenId === zineId) this.zinesDetail = detail;
    } catch (e) {
      if (this.zinesOpenId === zineId) {
        this.zinesDetailError =
          e instanceof AppError ? e.message : "Couldn't open that zine.";
      }
    }
    if (this.rail === "zines") this.render();
  }

  private async zinesSavePenName(): Promise<void> {
    const name = this.zinesPenNameDraft.trim();
    if (!name) {
      this.zinesPenNameError = "A pen name needs a byline.";
      this.render();
      return;
    }
    try {
      const view = await api.zinesSetPenName(name);
      if (view) this.zinesView = view;
      this.zinesPenNameOpen = false;
      this.zinesPenNameError = null;
      // A rename moves the byline on every zine this pen name wrote, and the
      // reading pane is rendering one of them from a detail fetched before the
      // rename. A held zine keeps its author's byline, so only an authored one
      // is patched.
      const open = this.zinesDetail?.zine;
      if (open?.authored) open.descriptor.pen_name = name;
    } catch (e) {
      this.zinesPenNameError =
        e instanceof AppError ? e.message : "Couldn't set that pen name.";
    }
    if (this.rail === "zines") this.render();
  }

  /**
   * The left column: identity and controls, never the corpus.
   *
   * The corpus used to be here as well — a section facet and a list of stories
   * — which made the pane a second, worse copy of the page beside it. What is
   * left is what a reader *does*: who they publish as, writing, and the two
   * switches over what the front page shows.
   */
  private renderZinesPane(): HTMLElement {
    const col = document.createElement("div");
    col.className = "zines-pane";

    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Zines";
    head.appendChild(h);
    col.appendChild(head);

    const penName = this.zinesView?.pen_name ?? null;
    const card = document.createElement("div");
    card.className = "zine-pen-card";
    if (penName) {
      const byline = document.createElement("div");
      byline.className = "zine-pen-name text-15";
      byline.textContent = penNameLabel(penName);
      const addr = document.createElement("div");
      addr.className = "zine-pen-id text-12 muted";
      addr.textContent = shortId(penName.id);
      addr.title = penName.id;
      const rename = document.createElement("button");
      rename.type = "button";
      rename.className = "zine-pen-rename pressable";
      rename.textContent = "Change";
      rename.onclick = () => {
        this.zinesPenNameOpen = true;
        this.zinesPenNameDraft = penName.name;
        this.zinesPenNameError = null;
        this.render();
      };
      card.append(byline, addr, rename);
    } else {
      const p = document.createElement("p");
      p.className = "zine-pen-id text-13 muted";
      p.textContent =
        "Choose a pen name to publish under. It is a separate key, kept with your vault, and it never touches your messaging identity.";
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "btn-primary pressable zine-pen-choose";
      btn.textContent = "Choose a pen name";
      btn.onclick = () => {
        this.zinesPenNameOpen = true;
        this.zinesPenNameDraft = "";
        this.zinesPenNameError = null;
        this.render();
      };
      card.append(p, btn);
    }
    col.appendChild(card);

    // Hidden while a draft is open, the way the Notes pane hides "New note": the
    // button starts an empty draft, so pressing it mid-draft silently threw away
    // whatever was typed. The draft page carries its own Publish and Discard.
    if (penName && !this.zinesEditing) {
      const write = document.createElement("button");
      write.type = "button";
      write.className = "btn-primary pressable zines-write";
      write.textContent = "Write a post";
      write.onclick = () => this.zinesStartNewPost();
      col.appendChild(write);
    }

    const controls = document.createElement("div");
    controls.className = "zines-controls";
    const controlsHead = document.createElement("h2");
    controlsHead.className = "zines-controls-head text-12 muted";
    controlsHead.textContent = "Show";
    controls.appendChild(controlsHead);
    controls.appendChild(
      this.zinesToggle("Mine only", this.zinesMineOnly, (on) =>
        this.zinesSetView(on, this.zinesShowMuted),
      ),
    );
    controls.appendChild(
      this.zinesToggle("Muted posts", this.zinesShowMuted, (on) =>
        this.zinesSetView(this.zinesMineOnly, on),
      ),
    );
    col.appendChild(controls);
    return col;
  }

  /**
   * One view control.
   *
   * A checkbox rather than a styled button: these are switches over what the
   * page shows, not buttons that do something once, and the native control is
   * the one that already says so — the same shape the Drops pane's toggles use.
   */
  private zinesToggle(
    label: string,
    on: boolean,
    onToggle: (on: boolean) => void,
  ): HTMLElement {
    const wrap = document.createElement("label");
    wrap.className = "zines-toggle text-13";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = on;
    cb.onchange = () => onToggle(cb.checked);
    wrap.append(cb, document.createTextNode(label));
    return wrap;
  }

  /**
   * The right column: the front page, one story, or the draft.
   *
   * This is where the corpus lives now. The pane beside it is controls.
   */
  private renderZinesPage(): HTMLElement {
    const page = document.createElement("div");
    page.className = "zines-page";
    if (this.zinesEditing) return this.renderZinesDraft(page);

    const id = this.zinesOpenId;
    if (id) return this.renderZinesReader(page, id);
    return this.renderZinesFront(page);
  }

  /**
   * The front page.
   *
   * A masthead, the corpus's own sections as a strip, one lead story, then
   * everything else under section headings. Browsing and reading are the same
   * surface, so there is no second list to keep in step with this one.
   */
  private renderZinesFront(page: HTMLElement): HTMLElement {
    page.appendChild(this.renderZinesMasthead());
    const corpus = this.renderZinesCorpusNote();
    if (corpus) page.appendChild(corpus);

    if (this.zinesError) {
      const err = document.createElement("p");
      err.className = "zines-note muted text-13";
      err.textContent = this.zinesError;
      page.appendChild(err);
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "btn-secondary pressable zines-retry";
      retry.textContent = "Try again";
      retry.onclick = () => void this.zinesLoad();
      page.appendChild(retry);
      return page;
    }
    if (this.zinesLoading && !this.zinesView) {
      const loading = document.createElement("p");
      loading.className = "zines-note muted text-13";
      loading.textContent = "Loading…";
      page.appendChild(loading);
      return page;
    }

    // The strip comes from the held corpus, not from what survives the section
    // filter, so an empty section still shows the way back to the whole page.
    if (this.zinesHeld().length) page.appendChild(this.renderZinesSections());
    const pen = this.zinesPen;
    if (pen) page.appendChild(this.renderZinesPenFilter(pen));

    const stories = this.zinesFiltered();
    if (!stories.length) {
      const empty = document.createElement("p");
      empty.className = "zines-note muted text-13";
      empty.textContent = this.zinesEmptyNote();
      page.appendChild(empty);
      return page;
    }

    // The lead is the newest story the reader has not muted; the rest of the
    // page is everything else, in the same newest-first order.
    const lead = leadOf(stories);
    if (!lead) return page; // Unreachable: an empty corpus returned above.
    const rest = stories.filter((z) => z !== lead);
    const leadBlock = document.createElement("section");
    leadBlock.className = "zines-lead";
    leadBlock.appendChild(this.renderZineCard(lead, true));
    page.appendChild(leadBlock);

    // With a section picked, the strip is the heading: grouping again would
    // print a section name over a page that is already only that section.
    if (this.zinesSection) {
      page.appendChild(this.renderZinesGrid(rest));
    } else {
      page.appendChild(this.renderZinesGroups(rest));
    }
    return page;
  }

  /** Why the page is empty, told apart by which control emptied it. */
  private zinesEmptyNote(): string {
    if (this.zinesPen) return `Nothing here is signed by ${this.zinesPen}.`;
    if (this.zinesSection) return `Nothing in ${this.zinesSection} yet.`;
    if (this.zinesMineOnly) return "You haven't published anything yet.";
    return "Nothing here yet. Write the first one, or wait for one to arrive.";
  }

  /**
   * The active byline filter, on the page it narrows.
   *
   * A filter a reader cannot see is a page that looks empty for no reason, and
   * this one is set from a story's byline — a page away from where it lands. So
   * it prints what it is and carries its own way out.
   */
  private renderZinesPenFilter(pen: string): HTMLElement {
    const bar = document.createElement("div");
    bar.className = "zines-pen-filter";
    const label = document.createElement("span");
    label.className = "text-13 muted";
    label.textContent = "Everything signed by this byline:";
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "zines-pen-chip pressable";
    chip.textContent = `${pen} ×`;
    chip.title = "Show every byline again";
    chip.onclick = () => this.zinesFilterByByline(null);
    bar.append(label, chip);
    return bar;
  }

  /**
   * The masthead: the page's name and today's date.
   *
   * Deliberately not the pen name. This page carries other people's zines at
   * least as much as this vault's, so it is a newsstand rather than one pen
   * name's paper — a byline belongs on the story, not over the whole page.
   */
  private renderZinesMasthead(): HTMLElement {
    const head = document.createElement("header");
    head.className = "zines-masthead";
    const name = document.createElement("h1");
    name.className = "zines-masthead-name";
    name.textContent = "Zines";
    const date = document.createElement("p");
    date.className = "zines-masthead-date";
    date.textContent = zinesDateLine();
    head.append(name, date);
    return head;
  }

  /**
   * What this vault is holding that no key has opened yet.
   *
   * Absent when there is nothing to say, because a status line that reads zero
   * on a healthy vault is noise. When there is something, it is stated plainly:
   * the bytes are kept on purpose rather than left as a mystery, they cost
   * space, and holding them is what keeps a post in the relay's window for the
   * readers who can open it.
   */
  private renderZinesCorpusNote(): HTMLElement | null {
    const view = this.zinesView;
    if (!view || !view.unreadable_blobs) return null;
    const note = document.createElement("p");
    note.className = "zines-corpus-note muted text-12";
    const held = view.held_blobs;
    note.textContent =
      `Holding ${held} sealed blob${held === 1 ? "" : "s"}, ` +
      `${view.unreadable_blobs} of which no key has opened yet. ` +
      "They are kept rather than dropped: a key may still arrive, and holding them is what keeps a post available to the readers who can open it.";
    return note;
  }

  /**
   * The section strip: the corpus's categories, on the page rather than in the
   * pane. Clicking one narrows the page to it; `All` clears the filter.
   */
  private renderZinesSections(): HTMLElement {
    const strip = document.createElement("nav");
    strip.className = "zines-sections";
    strip.appendChild(this.zinesSectionBtn("All", null, this.zinesHeld().length));
    for (const s of this.zinesSections()) {
      strip.appendChild(this.zinesSectionBtn(s.name, s.name, s.count));
    }
    return strip;
  }

  private zinesSectionBtn(
    label: string,
    section: string | null,
    count: number,
  ): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `zines-section-btn pressable${this.zinesSection === section ? " active" : ""}`;
    const name = document.createElement("span");
    name.textContent = label;
    const n = document.createElement("span");
    n.className = "zines-section-count";
    n.textContent = String(count);
    btn.append(name, n);
    btn.onclick = () => {
      this.zinesSection = section;
      // A new filter is a new page: open it at the masthead rather than at
      // wherever the last one happened to be scrolled to.
      this.pendingZinesScroll = null;
      this.render();
    };
    return btn;
  }

  /** Everything under the lead, filed by section, with the unsectioned last. */
  private renderZinesGroups(stories: ZineSummaryDto[]): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "zines-groups";
    const order = this.zinesSections().map((s) => s.name);
    for (const group of groupBySection(stories, order)) {
      const section = document.createElement("section");
      section.className = "zines-group";
      const head = document.createElement("h2");
      head.className = "zines-group-head";
      head.textContent = group.name ?? "Unsorted";
      section.append(head, this.renderZinesGrid(group.stories));
      wrap.appendChild(section);
    }
    return wrap;
  }

  /** A run of stories as cards. */
  private renderZinesGrid(stories: ZineSummaryDto[]): HTMLElement {
    const grid = document.createElement("div");
    grid.className = "zines-grid";
    for (const z of stories) grid.appendChild(this.renderZineCard(z, false));
    return grid;
  }

  /**
   * One story as the page shows it.
   *
   * The lead is the same content in a bigger treatment rather than a different
   * shape, so there is one place that decides what a story says about itself:
   * its section, whether it is yours, its headline, a taste of the body, and
   * the byline line.
   */
  private renderZineCard(z: ZineSummaryDto, lead: boolean): HTMLElement {
    const card = document.createElement("article");
    card.className = `zine-card pressable${lead ? " lead" : ""}${z.muted ? " muted-card" : ""}`;

    if (z.sections.length || z.authored) {
      const kicker = document.createElement("div");
      kicker.className = "zine-card-kicker";
      if (z.sections.length) {
        const section = document.createElement("span");
        section.className = "zine-card-section";
        section.textContent = z.sections[0];
        kicker.appendChild(section);
      }
      if (z.authored) {
        const mine = document.createElement("span");
        mine.className = "zine-card-mine";
        mine.textContent = "Yours";
        kicker.appendChild(mine);
      }
      card.appendChild(kicker);
    }

    const title = document.createElement("button");
    title.type = "button";
    title.className = "zine-card-title";
    title.textContent = z.title || "Untitled";
    title.onclick = () => void this.zinesOpen(z.zine_id);
    card.appendChild(title);

    if (z.preview) {
      const preview = document.createElement("p");
      preview.className = "zine-card-preview";
      preview.textContent = z.preview;
      card.appendChild(preview);
    }

    const meta = document.createElement("div");
    meta.className = "zine-card-meta";
    // The byline is a control, not text: it is the only place a reader can learn
    // a name they did not already know, so clicking it is how the page narrows
    // to that byline. A story with no byline yet has nothing to click.
    if (z.pen_name) {
      const byline = document.createElement("button");
      byline.type = "button";
      byline.className = "zine-byline pressable";
      byline.textContent = z.pen_name;
      byline.title = `Everything signed by ${z.pen_name}`;
      byline.onclick = () => this.zinesFilterByByline(z.pen_name);
      meta.appendChild(byline);
    }
    const bits: string[] = [];
    // A story whose blob has not been opened has no byline to click, but the
    // line still has to say so rather than start with a separator.
    if (!z.pen_name) bits.push(bylineOf(z));
    if (z.revision > 1) bits.push(`rev ${z.revision}`);
    if (z.comments) {
      bits.push(`${z.comments} comment${z.comments === 1 ? "" : "s"}`);
    }
    if (z.pending) {
      bits.push(`${z.pending} waiting for you`);
    }
    bits.push(readsTimeAgo(Math.floor(z.updated_ms / 1000)));
    if (z.muted) bits.push("muted");
    if (z.unreadable) bits.push("unreadable");
    if (bits.length) {
      const rest = document.createElement("span");
      rest.className = "zine-card-meta-rest";
      rest.textContent = bits.join(" · ");
      meta.appendChild(rest);
    }
    card.appendChild(meta);
    return card;
  }

  /**
   * One story on its own page, with the way back.
   *
   * The way back is on the page rather than in the pane because it belongs to
   * the story: a reader who wants to browse again is reading, not operating
   * something, and the pane is where operating happens.
   */
  private renderZinesReader(page: HTMLElement, id: string): HTMLElement {
    const reader = document.createElement("div");
    reader.className = "zines-reader";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary text-13 pressable zines-back";
    back.textContent = "← Front page";
    back.onclick = () => this.zinesBackToFront();
    reader.appendChild(back);

    if (this.zinesDetailError) {
      const err = document.createElement("p");
      err.className = "zines-note muted text-13";
      err.textContent = this.zinesDetailError;
      reader.appendChild(err);
      page.appendChild(reader);
      return page;
    }
    const detail = this.zinesDetail;
    if (!detail || detail.zine.descriptor.zine_id !== id) {
      const loading = document.createElement("p");
      loading.className = "zines-note muted text-13";
      loading.textContent = "Opening…";
      reader.appendChild(loading);
      page.appendChild(reader);
      return page;
    }
    reader.appendChild(this.renderZineArticle(detail));
    page.appendChild(reader);
    return page;
  }

  /** Back to the front page, where the reader left it. */
  private zinesBackToFront(): void {
    this.zinesOpenId = null;
    this.zinesDetail = null;
    this.zinesDetailError = null;
    this.zinesCommentHost = null;
    this.zinesCommentDraft = "";
    this.zinesCommentError = null;
    this.render();
  }

  private renderZineArticle(detail: ZineViewDto): HTMLElement {
    const { descriptor, history, authored, muted, unreadable, read_key } = detail.zine;
    const article = document.createElement("article");
    article.className = "zine-article";

    const head = document.createElement("header");
    head.className = "zine-article-head";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = descriptor.title || "Untitled";
    const byline = document.createElement("p");
    byline.className = "zine-article-byline text-13 muted";
    const bits: string[] = [];
    // The byline is the one control a reader can use to find the rest of what
    // this name wrote, so on the article it is a button rather than text.
    if (descriptor.pen_name) {
      const who = document.createElement("button");
      who.type = "button";
      who.className = "zine-byline pressable";
      who.textContent = descriptor.pen_name;
      who.title = `Everything signed by ${descriptor.pen_name}`;
      who.onclick = () => this.zinesFilterByByline(descriptor.pen_name);
      byline.appendChild(who);
    } else {
      byline.textContent = "unknown byline";
    }
    bits.push(readsTimeAgo(Math.floor(descriptor.created_ms / 1000)));
    if (descriptor.revision > 1) bits.push(`revision ${descriptor.revision}`);
    const rest = document.createElement("span");
    rest.className = "zine-article-byline-rest";
    rest.textContent = ` · ${bits.join(" · ")}`;
    byline.appendChild(rest);
    head.append(h, byline);

    if (descriptor.sections.length) {
      const tags = document.createElement("div");
      tags.className = "zine-article-tags";
      for (const s of descriptor.sections) {
        const tag = document.createElement("button");
        tag.type = "button";
        tag.className = "zine-tag pressable";
        tag.textContent = s;
        tag.onclick = () => {
          this.zinesSection = s;
          this.zinesOpenId = null;
          this.zinesDetail = null;
          // Landing on the front page under a new filter: it starts at the
          // masthead, not where this reader left the unfiltered page.
          this.pendingZinesScroll = null;
          this.render();
        };
        tags.appendChild(tag);
      }
      head.appendChild(tags);
    }
    article.appendChild(head);

    const actions = document.createElement("div");
    actions.className = "zine-article-actions";
    if (authored && !unreadable) {
      const revise = document.createElement("button");
      revise.type = "button";
      revise.className = "btn-secondary pressable";
      revise.textContent = "Write a new revision";
      revise.onclick = () =>
        this.zinesRevise(
          descriptor.zine_id,
          descriptor.title,
          descriptor.sections,
          descriptor.body,
        );
      actions.appendChild(revise);
    }
    if (authored) {
      const rename = document.createElement("button");
      rename.type = "button";
      rename.className = "btn-secondary pressable";
      rename.textContent = "Change pen name";
      rename.onclick = () => {
        this.zinesPenNameOpen = true;
        this.zinesPenNameDraft = this.zinesView?.pen_name?.name ?? descriptor.pen_name;
        this.zinesPenNameError = null;
        this.render();
      };
      actions.appendChild(rename);
    }
    const mute = document.createElement("button");
    mute.type = "button";
    mute.className = "btn-secondary pressable";
    mute.textContent = muted ? "Unmute" : "Mute";
    mute.title = muted
      ? "Count this zine again"
      : "Stop this zine being counted as new. It stays in your vault.";
    mute.onclick = () => void this.zinesToggleMute(descriptor.zine_id, !muted);
    actions.appendChild(mute);
    // Any holder may share, not just the author: the key confers read of a post
    // the sender could already read, so a member the author has no session with
    // can still be reached through whoever does. Offered only when there is a
    // key to hand over — an unreadable blob has none.
    if (!unreadable && read_key) {
      const share = document.createElement("button");
      share.type = "button";
      share.className = "btn-secondary pressable zine-share";
      share.textContent = "Share with…";
      share.title =
        "Hand this post's read key to one of your contacts, over the session you already have. They can pass it on in turn.";
      share.onclick = () => {
        this.zinesShareId = descriptor.zine_id;
        this.zinesShareContact = "";
        this.zinesShareError = null;
        this.render();
      };
      actions.appendChild(share);
    }
    if (!authored) {
      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "btn-secondary pressable zine-forget";
      forget.textContent = "Forget";
      forget.title =
        "Stop holding this zine. Not a retraction: other holders keep their copy.";
      forget.onclick = () => {
        this.zinesForgetId = descriptor.zine_id;
        this.render();
      };
      actions.appendChild(forget);
    }
    article.appendChild(actions);

    if (unreadable) {
      const held = document.createElement("p");
      held.className = "zines-note muted text-13";
      held.textContent =
        "This zine is held but not openable yet: the blob arrived before the key that unlocks it. It will open by itself once the key does.";
      article.appendChild(held);
      return article;
    }
    if (!read_key && !authored) {
      const held = document.createElement("p");
      held.className = "zines-note muted text-13";
      held.textContent = "This vault has no read key for this zine.";
      article.appendChild(held);
      return article;
    }

    const body = document.createElement("div");
    body.className = "zine-article-body";
    // Sanitized on the way in: the body is whatever a holder's vault stored,
    // and a holder is not trusted to have sanitized it.
    body.innerHTML = bodyToDisplayHtml(descriptor.body);
    article.appendChild(body);

    // The author's queue for this post, before the published thread: these are
    // the only comments that are not in the article yet, and merging one is what
    // puts it there.
    if (authored && detail.zine.pending_comments.length) {
      article.appendChild(this.renderZinePending(detail.zine.pending_comments));
    }

    if (descriptor.comments.length) {
      const thread = document.createElement("section");
      thread.className = "zine-comments";
      const th = document.createElement("h2");
      th.className = "text-17";
      th.textContent = `Comments (${descriptor.comments.length})`;
      thread.appendChild(th);
      for (const c of descriptor.comments) {
        const row = document.createElement("div");
        row.className = "zine-comment";
        const meta = document.createElement("div");
        meta.className = "zine-comment-meta text-12 muted";
        meta.textContent = `${c.author || "unknown"} · ${readsTimeAgo(Math.floor(c.created_ms / 1000))}`;
        const text = document.createElement("div");
        text.className = "zine-comment-body text-13";
        text.textContent = c.body;
        row.append(meta, text);
        thread.appendChild(row);
      }
      article.appendChild(thread);
    }

    // A comment is a reader's act, so the author does not get a box on their own
    // post: what they do about a comment is accept or drop it, above.
    if (!authored) article.appendChild(this.renderZineComposer());

    if (history.length) {
      const past = document.createElement("section");
      past.className = "zine-history";
      const ph = document.createElement("h2");
      ph.className = "text-17";
      ph.textContent = `Earlier revisions (${history.length})`;
      past.appendChild(ph);
      for (const r of history) {
        const row = document.createElement("div");
        row.className = "zine-history-row";
        const meta = document.createElement("div");
        meta.className = "zine-comment-meta text-12 muted";
        meta.textContent = `revision ${r.revision} · ${r.title || "Untitled"} · ${readsTimeAgo(Math.floor(r.replaced_ms / 1000))}`;
        row.appendChild(meta);
        const body = document.createElement("div");
        body.className = "zine-history-body text-13";
        body.innerHTML = bodyToDisplayHtml(r.body);
        row.appendChild(body);
        past.appendChild(row);
      }
      article.appendChild(past);
    }
    return article;
  }

  /**
   * The comments waiting for this author's decision, one row each.
   *
   * The copy is deliberate about what a refusal is: dropping a comment here
   * does not unsay it anywhere, because nothing was ever published. The blob is
   * on the lane, sealed to this vault alone, and other holders keep re-posting
   * it — which is the honest limit of author-side moderation.
   */
  private renderZinePending(comments: ZinePendingCommentDto[]): HTMLElement {
    const pile = document.createElement("section");
    pile.className = "zine-pending";
    const h = document.createElement("h2");
    h.className = "text-17";
    h.textContent = `Waiting for you (${comments.length})`;
    pile.appendChild(h);
    const note = document.createElement("p");
    note.className = "zines-note muted text-12";
    note.textContent =
      "Pulled from the lane and sealed to your pen name. Accepting one merges it and republishes this post; dropping one is not a retraction, because a refused comment was never published.";
    pile.appendChild(note);

    for (const c of comments) {
      const row = document.createElement("div");
      row.className = "zine-comment pending";
      const meta = document.createElement("div");
      meta.className = "zine-comment-meta text-12 muted";
      meta.textContent = `${c.author || "unknown"} · ${readsTimeAgo(Math.floor(c.created_ms / 1000))}`;
      const text = document.createElement("div");
      text.className = "zine-comment-body text-13";
      text.textContent = c.body;
      row.append(meta, text);

      const busy = this.zinesCommentBusyHash === c.hash;
      const buttons = document.createElement("div");
      buttons.className = "zine-pending-actions";
      const accept = document.createElement("button");
      accept.type = "button";
      accept.className = "btn-primary pressable zine-accept-comment";
      accept.textContent = busy ? "Merging…" : "Accept";
      accept.title =
        "Merge this comment into the thread and publish a new revision.";
      accept.disabled = busy;
      accept.onclick = () => void this.zinesAcceptComment(c.hash);
      const drop = document.createElement("button");
      drop.type = "button";
      drop.className = "btn-secondary pressable zine-discard-comment";
      drop.textContent = "Drop";
      drop.title = "Never merge this comment. Nothing is published either way.";
      drop.disabled = busy;
      drop.onclick = () => void this.zinesDiscardComment(c.hash);
      buttons.append(accept, drop);
      row.appendChild(buttons);
      pile.appendChild(row);
    }
    return pile;
  }

  /**
   * The comment box on a story this vault only holds.
   *
   * No session with the author is needed: the pen name is a public key that
   * travelled in the article, so the comment is sealed to it and published to
   * the lane. The box is built once per open story and re-parented on every
   * re-render — the same doctrine as the draft editor's host — so a poll cannot
   * take away a half-typed comment or the caret in it.
   */
  private renderZineComposer(): HTMLElement {
    const form = document.createElement("section");
    form.className = "zine-comment-form";
    const h = document.createElement("h2");
    h.className = "text-17";
    h.textContent = "Add a comment";
    const hint = document.createElement("p");
    hint.className = "zines-note muted text-12";
    hint.textContent =
      "Sealed to the author's pen name and published to the lane, so no session with them is needed. The author decides whether it joins the thread.";
    form.append(h, hint);

    if (!this.zinesCommentHost) {
      const box = document.createElement("textarea");
      box.className = "zine-comment-input";
      box.rows = 3;
      box.placeholder = "Write a comment…";
      box.value = this.zinesCommentDraft;
      box.oninput = () => {
        this.zinesCommentDraft = box.value;
      };
      this.zinesCommentHost = box;
    }
    const box = this.zinesCommentHost;
    box.disabled = this.zinesCommentBusy;
    form.appendChild(box);

    if (this.zinesCommentError) {
      const err = document.createElement("p");
      err.className = "zines-note zine-error text-12";
      err.textContent = this.zinesCommentError;
      form.appendChild(err);
    }

    const post = document.createElement("button");
    post.type = "button";
    post.className = "btn-primary pressable zine-comment-send";
    post.textContent = this.zinesCommentBusy ? "Publishing…" : "Publish comment";
    post.disabled = this.zinesCommentBusy;
    post.onclick = () => void this.zinesPostComment();
    form.appendChild(post);
    return form;
  }

  /** The draft form: title, sections, and the shared rich-text editor. */
  private renderZinesDraft(page: HTMLElement): HTMLElement {
    const form = document.createElement("div");
    form.className = "zine-draft";

    const head = document.createElement("div");
    head.className = "zine-draft-head";
    const h = document.createElement("h2");
    h.className = "text-17";
    h.textContent = this.zinesDraftZineId ? "New revision" : "New post";
    head.appendChild(h);
    const hint = document.createElement("p");
    hint.className = "muted text-12";
    hint.textContent = this.zinesDraftZineId
      ? "Published under your pen name and stored encrypted in your vault. Publishing adds a revision to this post's chain; earlier ones are kept."
      : "Published under your pen name and stored encrypted in your vault. Each post is its own zine, with its own read key and its own revision chain.";
    head.appendChild(hint);
    form.appendChild(head);

    const titleField = document.createElement("label");
    titleField.className = "zine-field";
    const titleLabel = document.createElement("span");
    titleLabel.className = "text-13";
    titleLabel.textContent = "Title";
    const titleInput = document.createElement("input");
    titleInput.type = "text";
    titleInput.className = "zine-title-input";
    titleInput.placeholder = "A title for this zine";
    titleInput.value = this.zinesDraftTitle;
    titleInput.oninput = () => {
      this.zinesDraftTitle = titleInput.value;
      titleInput.classList.remove("invalid");
    };
    titleField.append(titleLabel, titleInput);
    form.appendChild(titleField);

    const sectionField = document.createElement("label");
    sectionField.className = "zine-field";
    const sectionLabel = document.createElement("span");
    sectionLabel.className = "text-13";
    sectionLabel.textContent = "Sections";
    const sectionInput = document.createElement("input");
    sectionInput.type = "text";
    sectionInput.className = "zine-sections-input";
    sectionInput.placeholder = "rust, networking";
    sectionInput.value = this.zinesDraftSections;
    sectionInput.oninput = () => {
      this.zinesDraftSections = sectionInput.value;
    };
    const sectionHint = document.createElement("span");
    sectionHint.className = "muted text-12";
    sectionHint.textContent = "Comma-separated tags. They stay inside the vault.";
    sectionField.append(sectionLabel, sectionInput, sectionHint);
    form.appendChild(sectionField);

    // The editor host is built once per draft and re-parented on every
    // re-render, so the caret and the undo history survive a background repaint.
    if (!this.zinesEditorHost) {
      const host = document.createElement("div");
      host.className = "zine-editor";
      attachZineEditor(host, this.zinesDraftSeedHtml, () => this.zinesDraftChanged());
      this.zinesEditorHost = host;
    }
    form.appendChild(this.zinesEditorHost);

    const footer = document.createElement("div");
    footer.className = "zine-draft-actions";
    const publish = document.createElement("button");
    publish.type = "button";
    publish.className = "btn-primary pressable";
    publish.textContent = "Publish";
    // An untouched draft would append a revision identical to the last one, so
    // the button waits for a keystroke. Read from the store rather than a flag
    // of our own: a re-render can happen while the editor is already dirty.
    publish.disabled = !zineEditorIsDirty();
    publish.onclick = () => void this.zinesPublish(publish);
    this.zinesPublishBtn = publish;
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Discard";
    cancel.onclick = () => {
      this.zinesDiscardDraft();
      this.render();
    };
    footer.append(publish, cancel);
    form.appendChild(footer);
    page.appendChild(form);
    return page;
  }

  private renderZinesPenNameModal(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal zine-pen-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Pen name");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = this.zinesView?.pen_name ? "Change pen name" : "Choose a pen name";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-2)";
    p.textContent =
      "This is the byline readers see. The key behind it lives in your vault and is separate from your messaging identity, so changing the byline does not move the address your zine is published under.";
    modal.append(h, p);

    const field = document.createElement("label");
    field.className = "zine-field";
    const label = document.createElement("span");
    label.className = "text-13";
    label.textContent = "Byline";
    const input = document.createElement("input");
    input.type = "text";
    input.className = "zine-pen-input";
    input.placeholder = "How you sign your work";
    input.value = this.zinesPenNameDraft;
    input.oninput = () => {
      this.zinesPenNameDraft = input.value;
      this.zinesPenNameError = null;
    };
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void this.zinesSavePenName();
      }
    };
    field.append(label, input);
    modal.appendChild(field);

    if (this.zinesPenNameError) {
      const err = document.createElement("p");
      err.className = "zine-pen-error text-13";
      err.textContent = this.zinesPenNameError;
      modal.appendChild(err);
    }

    const row = document.createElement("div");
    row.className = "modal-actions";
    const save = document.createElement("button");
    save.type = "button";
    save.className = "btn-primary pressable";
    save.textContent = "Save";
    save.onclick = () => void this.zinesSavePenName();
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.zinesPenNameOpen = false;
      this.zinesPenNameError = null;
      this.render();
    };
    row.append(save, cancel);
    modal.appendChild(row);

    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.zinesPenNameOpen = false;
        this.zinesPenNameError = null;
        this.render();
      }
    };
    return back;
  }

  private renderZinesForgetModal(): HTMLElement {
    const zineId = this.zinesForgetId!;
    const row = this.zinesView?.zines.find((z) => z.zine_id === zineId);
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal zine-forget-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Forget this zine");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "Forget this zine?";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-2)";
    p.textContent = `${row?.title || "This zine"} is dropped from this vault. It is not a retraction: any other holder keeps their copy, and nothing already published is withdrawn.`;
    modal.append(h, p);

    const actions = document.createElement("div");
    actions.className = "modal-actions";
    const confirm = document.createElement("button");
    confirm.type = "button";
    confirm.className = "btn-primary pressable";
    confirm.textContent = "Forget";
    confirm.onclick = () => void this.zinesForget(zineId);
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.zinesForgetId = null;
      this.render();
    };
    actions.append(confirm, cancel);
    modal.appendChild(actions);

    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.zinesForgetId = null;
        this.render();
      }
    };
    return back;
  }

  /**
   * The share dialog: hand one contact this post's read key.
   *
   * The key travels over the session that already exists — the one thing in
   * Zines that is not the bulletin lane — so it works for a contact this vault
   * has a Signal session with, which is every contact in Trusts. What is being
   * given is described plainly: read access to this post, which the recipient
   * may pass on again.
   */
  private renderZinesShareModal(): HTMLElement {
    const zineId = this.zinesShareId!;
    const row = this.zinesView?.zines.find((z) => z.zine_id === zineId);
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal zine-share-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Share this zine");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "Share this zine";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-2)";
    p.textContent = `Send the read key for ${row?.title || "this zine"} to one contact. They will be able to read it and pass the key on in turn, and this is not a session with the author.`;
    modal.append(h, p);

    const field = document.createElement("div");
    field.className = "zine-field";
    const label = document.createElement("span");
    label.className = "text-13";
    label.textContent = "Contact";
    const picker = new ContactPicker({
      items: this.contacts.map((c) => ({
        nickname: c.nickname,
        label: c.mail_handle
          ? `${displayNick(c.nickname)} (${this.contactAddress(c)})`
          : displayNick(c.nickname),
        keywords: c.nickname,
      })),
      value: this.zinesShareContact,
      // Above the field, so the list cannot cover the Send button below it.
      dropUp: true,
      label: "Contact",
      placeholder: "Type a name…",
      emptyMessage: "No contacts yet. Add someone in Trusts first.",
      noMatchMessage: "No contacts match that name.",
      onChange: (nickname) => {
        this.zinesShareContact = nickname;
        this.zinesShareError = null;
        send.disabled = false;
      },
    });
    field.append(label, picker.element);
    modal.appendChild(field);

    if (this.zinesShareError) {
      const err = document.createElement("p");
      err.className = "zines-note zine-error text-13";
      err.textContent = this.zinesShareError;
      modal.appendChild(err);
    }

    const actions = document.createElement("div");
    actions.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.zinesShareId = null;
      this.zinesShareError = null;
      this.render();
    };
    const send = document.createElement("button");
    send.type = "button";
    send.className = "btn-primary pressable";
    send.textContent = this.zinesShareBusy ? "Sending…" : "Send key";
    send.disabled = this.zinesShareBusy || !this.zinesShareContact;
    send.onclick = () => void this.zinesShare();
    actions.append(cancel, send);
    modal.appendChild(actions);

    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.zinesShareId = null;
        this.zinesShareError = null;
        this.render();
      }
    };
    return back;
  }

  private renderReadsNav(): HTMLElement {
    const col = document.createElement("div");
    col.className = "reads-nav";

    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Reads";
    head.append(h);
    col.appendChild(head);

    const list = document.createElement("div");
    list.className = "reads-surface-list";
    for (const s of this.readsSources) {
      const row = document.createElement("div");
      row.className = "reads-source-row";
      const btn = document.createElement("button");
      btn.type = "button";
      const active = this.readsMode === "feed" && this.readsSource === s.id;
      btn.className = `reads-nav-btn pressable${active ? " active" : ""}`;
      btn.textContent = s.title;
      btn.title = s.url;
      btn.onclick = () => this.readsOpenSource(s.id);
      row.appendChild(btn);
      if (!s.built_in) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "reads-source-remove btn-icon-round pressable";
        remove.setAttribute("aria-label", `Remove ${s.title}`);
        remove.textContent = "×";
        remove.onclick = () => void this.readsRemoveFeed(s.id);
        row.appendChild(remove);
      }
      list.appendChild(row);
    }

    // A dialog rather than an inline field: the field was cramped in the nav,
    // and its errors rendered over in the middle column where nobody looked —
    // so a refused feed read as "nothing happened". The dialog keeps the URL,
    // its validation, and its reason together.
    const addBtn = document.createElement("button");
    addBtn.type = "button";
    addBtn.className = "reads-add-feed pressable";
    addBtn.appendChild(icon("plus"));
    addBtn.appendChild(document.createTextNode("Add a feed"));
    addBtn.onclick = () => {
      this.readsAddOpen = true;
      this.readsAddError = null;
      this.render();
    };
    list.appendChild(addBtn);

    const savedBtn = document.createElement("button");
    savedBtn.type = "button";
    savedBtn.className = `reads-nav-btn pressable${this.readsMode === "saved" ? " active" : ""}`;
    savedBtn.textContent = "Saved";
    void getReadsState().then((state) => {
      const n = state.saved.length;
      if (n > 0 && this.rail === "reads") {
        const count = savedBtn.querySelector(".reads-nav-count");
        if (count) count.textContent = String(n);
      }
    });
    const savedCount = document.createElement("span");
    savedCount.className = "reads-nav-count";
    savedBtn.appendChild(savedCount);
    savedBtn.onclick = () => this.readsOpenSaved();
    list.appendChild(savedBtn);
    col.appendChild(list);
    return col;
  }

  private renderReadsList(): HTMLElement {
    const col = document.createElement("div");
    col.className = "reads-list";

    const head = document.createElement("div");
    head.className = "conv-list-header";
    const title = document.createElement("span");
    title.className = "text-15 muted";
    title.textContent = this.readsMode === "saved" ? "Saved" : this.readsSourceTitle;
    head.appendChild(title);
    col.appendChild(head);

    if (this.readsError) {
      const err = document.createElement("p");
      err.className = "reads-status muted text-13";
      err.textContent = this.readsError;
      col.appendChild(err);
    }
    if (this.readsLoading && !this.readsStories.length) {
      const loading = document.createElement("p");
      loading.className = "reads-status muted text-13";
      loading.textContent = "Loading…";
      col.appendChild(loading);
      return col;
    }

    const stories = this.readsStories;
    if (!stories.length) {
      const empty = document.createElement("p");
      empty.className = "reads-status muted text-13";
      empty.textContent =
        this.readsMode === "saved"
          ? "Nothing saved yet — star a story to read it later."
          : "Nothing here right now — check back soon.";
      col.appendChild(empty);
    }
    for (const s of stories) {
      col.appendChild(this.renderReadsRow(s));
    }

    if (this.readsMode !== "saved" && !this.readsDone && !this.readsLoading) {
      const more = document.createElement("button");
      more.type = "button";
      more.className = "btn-primary pressable reads-more";
      more.textContent = "More";
      more.onclick = () => void this.readsLoad(this.readsPage + 1);
      col.appendChild(more);
    }
    if (this.readsLoading && this.readsStories.length) {
      const loading = document.createElement("p");
      loading.className = "reads-status muted text-13";
      loading.textContent = "Loading…";
      col.appendChild(loading);
    }
    return col;
  }

  private renderReadsRow(s: ReadsStoryDto & { saved: boolean; read: boolean }): HTMLElement {
    const row = document.createElement("article");
    row.className = `reads-row pressable${s.read ? " read" : ""}${this.readsOpenId === s.id ? " open" : ""}`;

    const top = document.createElement("div");
    top.className = "reads-row-top";
    const title = document.createElement("button");
    title.type = "button";
    title.className = "reads-row-title text-15";
    title.textContent = s.title;
    title.onclick = () => this.readsOpenStory(s.id);
    top.appendChild(title);

    const actions = document.createElement("div");
    actions.className = "reads-row-actions";
    const save = document.createElement("button");
    save.type = "button";
    save.className = "btn-icon-round pressable";
    save.setAttribute("aria-label", s.saved ? "Remove from Saved" : "Save for later");
    const saveIcon = icon("star");
    if (s.saved) saveIcon.classList.add("active-star");
    save.appendChild(saveIcon);
    save.onclick = (ev) => {
      ev.stopPropagation();
      void this.readsSave(s.id);
    };
    const hide = document.createElement("button");
    hide.type = "button";
    hide.className = "btn-icon-round pressable";
    hide.setAttribute("aria-label", "Hide this story");
    hide.appendChild(icon("hide"));
    hide.onclick = (ev) => {
      ev.stopPropagation();
      void this.readsHide(s.id);
    };
    actions.append(save, hide);
    top.appendChild(actions);
    row.appendChild(top);

    const meta = document.createElement("div");
    meta.className = "reads-row-meta text-12 muted";
    const parts: string[] = [s.author, readsTimeAgo(s.created_at)];
    if (s.hn_url) {
      // HN story: points and discussion size mean something here.
      parts.unshift(`${s.points} points`);
      parts.push(`${s.num_comments} comment${s.num_comments === 1 ? "" : "s"}`);
    }
    if (s.domain) parts.push(s.domain);
    const metaText = document.createElement("span");
    metaText.textContent = parts.join(" · ");
    meta.appendChild(metaText);
    row.appendChild(meta);
    return row;
  }

  private renderReadsMain(): HTMLElement {
    const main = document.createElement("div");
    main.className = "reads-main";
    const id = this.readsOpenId;
    if (!id) {
      const empty = document.createElement("div");
      empty.className = "reads-empty";
      empty.appendChild(icon("reads"));
      const p = document.createElement("p");
      p.className = "muted text-15";
      p.textContent = "Pick something to read.";
      empty.appendChild(p);
      main.appendChild(empty);
      return main;
    }
    // HN stories resolve to their detail (points, comments); RSS items only
    // exist as list rows — the reader is the whole story.
    const detail = this.readsDetail;
    const s = detail?.story ?? this.readsStories.find((x) => x.id === id);
    if (!s) {
      if (this.readsDetailLoading) {
        const loading = document.createElement("p");
        loading.className = "reads-status muted text-13";
        loading.textContent = "Loading…";
        main.appendChild(loading);
        return main;
      }
      const empty = document.createElement("div");
      empty.className = "reads-empty";
      empty.appendChild(icon("reads"));
      const p = document.createElement("p");
      p.className = "muted text-15";
      p.textContent = "Pick something to read.";
      empty.appendChild(p);
      main.appendChild(empty);
      return main;
    }

    const head = document.createElement("header");
    head.className = "reads-article-head";
    const h1 = document.createElement("h1");
    h1.className = "text-22";
    h1.textContent = s.title;
    head.appendChild(h1);

    const meta = document.createElement("p");
    meta.className = "reads-row-meta text-12 muted";
    const parts: string[] = [s.author, readsTimeAgo(s.created_at)];
    if (s.hn_url) {
      parts.unshift(`${s.points} points`);
      parts.push(`${s.num_comments} comment${s.num_comments === 1 ? "" : "s"}`);
    }
    meta.textContent = parts.join(" · ");
    head.appendChild(meta);

    const actions = document.createElement("div");
    actions.className = "reads-article-actions";
    const viewBtn = (label: string, view: "reader" | "comments", enabled: boolean) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = `reads-view-btn pressable${this.readsView === view ? " active" : ""}`;
      b.disabled = !enabled;
      b.textContent = label;
      b.onclick = () => this.readsSwitchView(view);
      return b;
    };
    actions.append(
      viewBtn("Read here", "reader", Boolean(s.url)),
      // Only HN stories have a discussion behind them.
      viewBtn(`Discussion (${s.num_comments})`, "comments", Boolean(s.hn_url)),
    );

    const save = document.createElement("button");
    save.type = "button";
    save.className = "reads-view-btn pressable";
    const savedNow = this.readsStories.find((x) => x.id === s.id)?.saved ?? false;
    save.textContent = savedNow ? "★ Saved" : "☆ Save";
    save.onclick = () => void this.readsSave(s.id);
    actions.appendChild(save);

    if (s.url) {
      const open = document.createElement("button");
      open.type = "button";
      open.className = "reads-view-btn pressable";
      open.appendChild(icon("externalLink"));
      const span = document.createElement("span");
      span.textContent = "Open original";
      open.appendChild(span);
      open.onclick = () => this.readsConfirmOpen(s.url!);
      actions.appendChild(open);
    }
    head.appendChild(actions);
    main.appendChild(head);

    if (this.readsView === "reader") {
      if (this.readsReaderLoading) {
        const p = document.createElement("p");
        p.className = "reads-status muted text-13";
        p.textContent = "Fetching the article…";
        main.appendChild(p);
      } else if (this.readsReaderError) {
        main.appendChild(
          this.readsReaderUnavailable(s.url ?? null, this.readsReaderError),
        );
      } else if (this.readsReaderHtml !== null) {
        main.appendChild(this.readsSanitizedPane(this.readsReaderHtml, "reads-article"));
      }
      return main;
    }

    // Discussion view: the story's own text (Ask HN) above the comment tree.
    if (!detail) {
      const loading = document.createElement("p");
      loading.className = "reads-status muted text-13";
      loading.textContent = "Loading…";
      main.appendChild(loading);
      return main;
    }
    const thread = document.createElement("div");
    thread.className = "reads-thread";
    if (detail.text_html) {
      thread.appendChild(this.readsSanitizedPane(detail.text_html, "reads-story-text"));
    }
    if (!detail.comments.length) {
      const none = document.createElement("p");
      none.className = "reads-status muted text-13";
      none.textContent = "No comments yet.";
      thread.appendChild(none);
    }
    for (const c of detail.comments) {
      thread.appendChild(this.renderReadsComment(c, 0));
    }
    main.appendChild(thread);
    return main;
  }

  /**
   * The reader could not fetch this page.
   *
   * The common cause is a bot challenge (Cloudflare and friends) that no header
   * tweak gets past, so it is shown as a deliberate, centered state with the way
   * out — open the original in a real browser — as the one obvious action. A
   * muted one-liner under the article head read as a bug and left the reader
   * with no clear move.
   */
  private readsReaderUnavailable(url: string | null, reason: string): HTMLElement {
    const box = document.createElement("div");
    box.className = "reads-empty reads-unavailable";
    box.appendChild(icon("externalLink"));

    // The daemon's `Blocked` reason is the one case worth its own words; every
    // other failure (no content, timeout) shows its reason as-is.
    const blocked = reason.includes("automated reader");
    const h = document.createElement("p");
    h.className = "text-17";
    h.textContent = blocked
      ? "This site blocks automated readers"
      : "This page could not be loaded";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent = blocked
      ? "It only serves its pages to a real browser. You can still read it in yours."
      : reason;
    box.append(h, p);

    if (url) {
      const open = document.createElement("button");
      open.type = "button";
      open.className = "btn-primary pressable reads-unavailable-open";
      open.appendChild(icon("externalLink"));
      const span = document.createElement("span");
      span.textContent = "Open the original";
      open.appendChild(span);
      open.onclick = () => this.readsConfirmOpen(url);
      box.appendChild(open);
    }
    return box;
  }

  /**
   * The only place in the pane where daemon-sanitized network HTML is
   * injected. Clicks on links are intercepted for the confirm step; nothing
   * navigates the webview itself.
   */
  private readsSanitizedPane(html: string, cls: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = cls;
    pane.innerHTML = html;
    pane.addEventListener("click", (ev) => {
      const target = ev.target as HTMLElement | null;
      const a = target?.closest?.("a[href]") as HTMLAnchorElement | null;
      if (!a) return;
      ev.preventDefault();
      this.readsConfirmOpen(a.href);
    });
    return pane;
  }

  private renderReadsComment(c: ReadsCommentDto, depth: number): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "reads-comment";
    wrap.style.setProperty("--depth", String(depth));

    const head = document.createElement("button");
    head.type = "button";
    head.className = "reads-comment-head text-12 muted pressable";
    const collapsed = this.readsCollapsed.has(c.id);
    head.textContent = `${collapsed ? "▸" : "▾"} ${c.author} · ${readsTimeAgo(c.created_at)}`;
    head.setAttribute("aria-label", `${collapsed ? "Expand" : "Collapse"} comment by ${c.author}`);
    head.onclick = () => {
      if (this.readsCollapsed.has(c.id)) this.readsCollapsed.delete(c.id);
      else this.readsCollapsed.add(c.id);
      this.render();
    };
    wrap.appendChild(head);

    if (!collapsed) {
      if (c.html) wrap.appendChild(this.readsSanitizedPane(c.html, "reads-comment-body"));
      for (const child of c.children) {
        wrap.appendChild(this.renderReadsComment(child, Math.min(depth + 1, 8)));
      }
    }
    return wrap;
  }

  /**
   * The "Add a feed" dialog.
   *
   * The daemon validates a feed by fetching it once, so the URL, the in-flight
   * state, and the reason a feed was refused all live here together — instead
   * of a cramped nav field whose error rendered over in the middle column.
   */
  private renderReadsAddModal(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    back.onclick = (e) => {
      if (e.target === back) this.closeReadsAdd();
    };
    const modal = document.createElement("div");
    modal.className = "modal reads-add-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Add an RSS feed");

    const title = document.createElement("h2");
    title.className = "text-22";
    title.textContent = "Add an RSS feed";

    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.textContent =
      "Paste the feed's address — RSS, Atom, and JSON Feed all work. Without a scheme, https:// is assumed.";

    const err = document.createElement("p");
    err.className = "reads-add-error text-13";
    err.hidden = !this.readsAddError;
    err.textContent = this.readsAddError ?? "";

    const input = document.createElement("input");
    input.className = "input-field reads-add-field";
    input.type = "text";
    input.placeholder = "https://example.com/feed.xml";
    input.setAttribute("aria-label", "RSS feed URL");
    input.value = this.readsSourceDraft;
    input.disabled = this.readsSourceBusy;
    input.autocomplete = "off";
    input.spellcheck = false;
    input.oninput = () => {
      this.readsSourceDraft = input.value;
      // Clear the reason as soon as the URL changes: it belonged to the last try.
      if (this.readsAddError) {
        this.readsAddError = null;
        err.hidden = true;
        err.textContent = "";
      }
    };
    input.onkeydown = (ev) => {
      if (ev.key === "Enter") {
        ev.preventDefault();
        void this.readsAddFeed();
      }
    };

    const actions = document.createElement("div");
    actions.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => this.closeReadsAdd();
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-primary pressable";
    add.textContent = this.readsSourceBusy ? "Adding…" : "Add feed";
    add.disabled = this.readsSourceBusy;
    add.onclick = () => void this.readsAddFeed();
    actions.append(cancel, add);

    modal.append(title, hint, input, err, actions);
    back.appendChild(modal);

    // Focus once the node is in the document, so paste-and-Enter is the path.
    queueMicrotask(() => {
      if (this.readsAddOpen) input.focus();
    });
    return back;
  }

  private closeReadsAdd(): void {
    this.readsAddOpen = false;
    this.readsAddError = null;
    this.render();
  }

  private renderReadsConfirm(): HTMLElement {
    const url = this.readsConfirmUrl!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", "Open external link");
    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "Open outside Hush?";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent = `This leaves the app and opens ${this.readsHostOf(url)} in your browser. The site will see your normal browsing identity, not Hush's.`;
    const row = document.createElement("div");
    row.className = "reads-confirm-row";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Stay here";
    cancel.onclick = () => {
      this.readsConfirmUrl = null;
      this.render();
    };
    const open = document.createElement("button");
    open.type = "button";
    open.className = "btn-primary pressable";
    open.textContent = "Open in browser";
    open.onclick = () => {
      this.readsConfirmUrl = null;
      if (isTauriShell()) {
        void invoke("hush_open_url", { url }).catch(() => {
          /* the modal already closed; the failure is visible by nothing happening */
        });
      } else {
        window.open(url, "_blank", "noopener,noreferrer");
      }
      this.render();
    };
    row.append(cancel, open);
    modal.append(h, p, row);
    back.appendChild(modal);
    return back;
  }

  private renderMailFolders(): HTMLElement {
    const col = document.createElement("div");
    col.className = "mail-folders";

    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Mail";
    const compose = document.createElement("button");
    compose.type = "button";
    compose.className = "btn-icon-round pressable";
    compose.appendChild(icon("plus"));
    compose.setAttribute("aria-label", "Compose");
    compose.onclick = () => this.openCompose({});
    head.append(h, compose);

    const addr = document.createElement("p");
    addr.className = "muted text-12 mail-address";
    addr.textContent =
      this.mailAddress() ?? "No address yet — set one in Settings → Profile.";
    col.append(head, addr);

    const flags = this.mailPrefs();
    const folders: { id: MailFolder; ic: IconName }[] = [
      { id: "inbox", ic: "inbox" },
      { id: "drops", ic: "drop" },
      { id: "sent", ic: "send" },
      { id: "outbox", ic: "outbox" },
      { id: "drafts", ic: "drafts" },
      { id: "starred", ic: "star" },
      { id: "archive", ic: "archive" },
      { id: "trash", ic: "trash" },
    ];

    const list = document.createElement("div");
    list.className = "mail-folder-list";
    for (const f of folders) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `mail-folder pressable${this.mailFolder === f.id ? " active" : ""}`;
      btn.appendChild(icon(f.ic));
      const label = document.createElement("span");
      label.className = "mail-folder-name";
      label.textContent = MAIL_FOLDER_LABEL[f.id];
      btn.appendChild(label);

      // Inbox shows how much is unread; Drops likewise (unopened, and only
      // what the folder shows); the rest show how much is in them.
      const unread =
        f.id === "inbox"
          ? unreadCount(flags)
          : f.id === "drops"
            ? listDropInbox(flags).filter((d) => isDropUnread(d, flags)).length
            : 0;
      const count = unread > 0 ? unread : folderCount(f.id, flags);
      if (count > 0) {
        const badge = document.createElement("span");
        badge.className = unread > 0 ? "mail-folder-count unread" : "mail-folder-count";
        badge.textContent = String(count);
        btn.appendChild(badge);
      }
      btn.onclick = () => this.openFolder(f.id);
      list.appendChild(btn);
    }
    col.appendChild(list);
    return col;
  }

  private openFolder(folder: MailFolder): void {
    this.mailFolder = folder;
    this.mailThreadId = null;
    this.mailOpenId = null;
    this.dropInboxOpenId = null;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    if (folder === "drops" && !this.dropInboxKey) {
      // First visit: learn whether a drop key exists at all, so the empty
      // state can say the useful thing ("publish one") rather than a shrug.
      void api.dropsKey().then((k) => {
        this.dropInboxKey = k;
        if (this.mailFolder === "drops") this.render();
      });
    }
    this.render();
  }

  // ----------------------------------------------------------- main pane

  private renderMailMain(): HTMLElement {
    if (this.mailComposeOpen) return this.renderMailCompose();
    if (this.mailFolder === "drops") {
      return this.dropInboxOpenId
        ? this.renderDropView(this.dropInboxOpenId)
        : this.renderDropList();
    }
    if (this.mailThreadId) return this.renderMailThread(this.mailThreadId);
    return this.renderMailList();
  }

  private renderMailList(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";

    const bar = document.createElement("div");
    bar.className = "mail-toolbar";
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = MAIL_FOLDER_LABEL[this.mailFolder];
    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const inp = document.createElement("input");
    inp.placeholder = "Search mail";
    inp.value = this.mailSearch;
    inp.oninput = () => {
      this.mailSearch = inp.value;
      this.render();
    };
    search.appendChild(inp);
    bar.append(title, search);
    pane.appendChild(bar);

    if (this.mailFolder === "drafts") {
      pane.appendChild(this.renderDraftList());
      return pane;
    }

    const flags = this.mailPrefs();
    const rows = searchMessages(
      listFolder(this.mailFolder, flags),
      this.mailSearch,
    );
    // The shared folders file drops too — Trash, Archive, and Starred list
    // them beside mail, newest first across both kinds.
    const needle = this.mailSearch.trim().toLowerCase();
    const dropRows = listDropsInFolder(this.mailFolder, flags).filter(
      (d) =>
        !needle ||
        (d.subject ?? "").toLowerCase().includes(needle) ||
        d.body.toLowerCase().includes(needle),
    );
    if (!rows.length && !dropRows.length) {
      pane.appendChild(
        this.mailEmpty(
          this.mailSearch
            ? "No mail matches that search."
            : MAIL_EMPTY[this.mailFolder],
        ),
      );
      return pane;
    }

    // One list, one chronology: mail and drops interleaved by when each
    // arrived, so a folder reads as a single history.
    const items: { ts: number; el: HTMLElement }[] = [
      ...rows.map((m) => ({ ts: m.createdAt, el: this.mailRow(m, flags) })),
      ...dropRows.map((d) => ({ ts: d.receivedAt, el: this.dropRow(d, flags) })),
    ].sort((a, b) => b.ts - a.ts);

    const list = document.createElement("div");
    list.className = "mail-list";
    for (const it of items) list.appendChild(it.el);
    pane.appendChild(list);
    return pane;
  }

  /** One row in the message list: who, subject, snippet, when. */
  private mailRow(m: MailMessage, flags: MailFlags): HTMLElement {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `mail-row pressable${isUnread(m, flags) ? " unread" : ""}`;

    const dot = document.createElement("span");
    dot.className = "mail-row-dot";

    const who = document.createElement("span");
    who.className = "mail-row-who";
    who.textContent = m.outgoing
      ? `To ${displayNick(m.contactNick)}`
      : displayNick(m.contactNick);

    const main = document.createElement("div");
    main.className = "mail-row-main";
    const subject = document.createElement("div");
    subject.className = "mail-row-subject";
    subject.textContent = m.subject || "(no subject)";
    const snip = document.createElement("div");
    snip.className = "mail-row-snippet";
    snip.textContent = preview(m);
    main.append(subject, snip);

    const right = document.createElement("div");
    right.className = "mail-row-right";
    const time = document.createElement("span");
    time.className = "text-12 faint";
    time.textContent = formatRelative(m.createdAt);
    right.appendChild(time);
    if (m.state === "queued") {
      const queued = document.createElement("span");
      queued.className = "mail-row-queued";
      queued.textContent = "Queued";
      right.appendChild(queued);
    }
    if (flags.starred.has(m.id)) right.appendChild(this.iconSmall("star"));
    if (m.attachments.length) right.appendChild(this.iconSmall("attach"));

    row.append(dot, who, main, right);
    row.onclick = () => this.openMessage(m);
    return row;
  }

  private renderDraftList(): HTMLElement {
    const drafts = listDrafts();
    if (!drafts.length) return this.mailEmpty(MAIL_EMPTY.drafts);

    const list = document.createElement("div");
    list.className = "mail-list";
    for (const d of drafts) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "mail-row pressable";

      const dot = document.createElement("span");
      dot.className = "mail-row-dot";

      const who = document.createElement("span");
      who.className = "mail-row-who";
      who.textContent = d.to ? `To ${displayNick(d.to)}` : "No recipient";

      const main = document.createElement("div");
      main.className = "mail-row-main";
      const subject = document.createElement("div");
      subject.className = "mail-row-subject";
      subject.textContent = d.subject || "(no subject)";
      const snip = document.createElement("div");
      snip.className = "mail-row-snippet";
      snip.textContent = draftPreview(d);
      main.append(subject, snip);

      const right = document.createElement("div");
      right.className = "mail-row-right";
      const time = document.createElement("span");
      time.className = "text-12 faint";
      time.textContent = formatRelative(d.updatedAt);
      right.appendChild(time);
      if (d.attachments.length) right.appendChild(this.iconSmall("attach"));

      row.append(dot, who, main, right);
      row.onclick = () => this.openCompose({ draftId: d.id });
      list.appendChild(row);
    }
    return list;
  }

  /** Open a message: show its conversation, and mark what you have seen read. */
  private openMessage(m: MailMessage): void {
    this.mailThreadId = m.threadId;
    this.mailOpenId = m.id;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.render();
    // The read flags are a prefs write, so the pane opens first and the unread
    // dot clears a moment later rather than holding up the click.
    void this.markThreadRead(m.threadId).then(() => {
      if (this.mailThreadId === m.threadId) this.render();
    });
  }

  private async markThreadRead(threadId: string): Promise<void> {
    const flags = this.mailPrefs();
    const unseen = getMailThread(threadId).filter((m) => isUnread(m, flags));
    if (!unseen.length) return;
    const read = new Set(flags.read);
    for (const m of unseen) read.add(m.id);
    await patchUiPrefs({ mail_read: [...read] });
  }

  private renderMailThread(threadId: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    const msgs = getMailThread(threadId);
    const contact =
      msgs.find((m) => !m.outgoing)?.contactNick ?? msgs[0]?.contactNick ?? "";
    const flags = this.mailPrefs();
    // Actions apply to the message you clicked, falling back to the newest one,
    // which is at the top of the thread.
    const targetId = this.mailOpenId ?? msgs[0]?.id ?? "";

    const head = document.createElement("div");
    head.className = "mail-read-head";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-icon-round pressable";
    back.appendChild(icon("back"));
    back.setAttribute("aria-label", `Back to ${MAIL_FOLDER_LABEL[this.mailFolder]}`);
    back.onclick = () => {
      this.mailThreadId = null;
      this.mailOpenId = null;
      this.render();
    };

    const titles = document.createElement("div");
    titles.className = "mail-read-titles";
    const subject = threadSubject(threadId);
    const h = document.createElement("h1");
    h.className = "text-17";
    h.textContent = subject;
    const sub = document.createElement("div");
    sub.className = "muted text-13";
    sub.textContent = `With ${displayNick(contact)} · ${msgs.length} message${msgs.length === 1 ? "" : "s"}`;
    titles.append(h, sub);

    const actions = document.createElement("div");
    actions.className = "mail-read-actions";
    if (flags.trashed.has(targetId)) {
      actions.append(
        this.mailAction("Restore to Inbox", "inbox", () =>
          void this.flagMail("mail_trashed", targetId, false),
        ),
        this.mailAction("Delete forever", "trash", () =>
          void this.deleteMailForever(targetId),
        ),
      );
    } else {
      const starred = flags.starred.has(targetId);
      actions.append(
        this.mailAction(starred ? "Unstar" : "Star", "star", () =>
          void this.flagMail("mail_starred", targetId, !starred), starred),
        this.mailAction("Archive", "archive", () =>
          void this.flagMail("mail_archived", targetId, true),
        ),
        this.mailAction("Move to Trash", "trash", () =>
          void this.flagMail("mail_trashed", targetId, true),
        ),
      );
    }
    actions.appendChild(
      this.mailAction("Reply", "reply", () =>
        this.openCompose({
          to: contact,
          threadId,
          subject: reSubject(threadSubject(threadId)),
        }),
      ),
    );
    // Forward quotes the one message the actions target, not the whole thread.
    const target = msgs.find((m) => m.id === targetId) ?? msgs[0];
    if (target) {
      actions.appendChild(
        this.mailAction("Forward", "send", () => this.forwardMail(target)),
      );
    }
    head.append(back, titles, actions);
    pane.appendChild(head);

    const list = document.createElement("div");
    list.className = "mail-messages";
    for (const m of msgs) {
      const card = document.createElement("article");
      card.className = `mail-message${m.outgoing ? " outgoing" : ""}${m.id === this.mailOpenId ? " open" : ""}`;

      // Classic mail layout: a fixed avatar column, then everything else. The
      // message is not a card — messages are separated by a rule, and the avatar
      // is what tells you who wrote it without reading the header.
      // The avatar is the author: the contact for mail that arrived, you for
      // mail you sent (the same key `avatarKeyFor` already picks).
      const author = m.outgoing
        ? (this.mailHandle ?? this.vaultName)
        : displayNick(m.contactNick);
      card.appendChild(createAvatarEl(this.avatarKeyFor(m), "sm", false, author));

      const content = document.createElement("div");
      content.className = "mail-message-content";

      // A genuine mail header: the fields a client shows, one per line, with
      // the labels forming a left column so the values line up. The subject
      // belongs to the thread, not this message, so it repeats down the stack.
      const msgHead = document.createElement("dl");
      msgHead.className = "mail-message-head";
      msgHead.append(
        ...this.mailHeaderField(
          "From",
          m.outgoing ? (this.mailAddress() ?? "You") : displayNick(m.contactNick),
          "mail-message-from",
        ),
        ...this.mailHeaderField(
          "To",
          m.outgoing
            ? (this.contactByNick(m.contactNick)
                ? this.contactAddress(this.contactByNick(m.contactNick)!)
                : displayNick(m.contactNick))
            : (this.mailAddress() ?? "you"),
        ),
        ...this.mailHeaderField("Date", this.mailDateEl(m.createdAt)),
        ...this.mailHeaderField("Subject", subject || "(no subject)"),
      );

      // A rule under the header is what separates the envelope from the letter.
      const rule = document.createElement("hr");
      rule.className = "mail-message-rule";

      const body = document.createElement("div");
      body.className = "text-15 mail-message-body";
      // The body may be rich HTML from the composer, or plain text from an
      // older record. `bodyToDisplayHtml` sanitizes either way.
      body.innerHTML = bodyToDisplayHtml(m.body);
      this.guardLinks(body);

      content.append(msgHead, rule, body);
      if (m.attachments.length) {
        // Attachments are their own section, not text that happens to sit under
        // the message, so a second rule introduces them.
        const attachRule = document.createElement("hr");
        attachRule.className = "mail-message-rule";
        content.appendChild(attachRule);
        for (const a of m.attachments) content.appendChild(this.mailAttachmentChip(a));
      }
      card.appendChild(content);
      list.appendChild(card);
    }
    pane.appendChild(list);
    return pane;
  }

  /**
   * One labelled line of a mail message header.
   *
   * Returns the `<dt>`/`<dd>` pair so the caller can append them in order; the
   * grid on `.mail-message-head` lays each pair out as a label column and a
   * value column. `valueClass` lets a single field (the sender) stand out.
   */
  private mailHeaderField(
    label: string,
    value: Node | string,
    valueClass?: string,
  ): [HTMLElement, HTMLElement] {
    const dt = document.createElement("dt");
    dt.className = "mail-message-label";
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.className = "mail-message-value";
    if (valueClass) dd.classList.add(valueClass);
    if (typeof value === "string") dd.textContent = value;
    else dd.appendChild(value);
    return [dt, dd];
  }

  /**
   * The moment a message was sent, as a real timestamp.
   *
   * A mail header shows the date; the relative form is kept on `title` so the
   * "3h ago" reading is still one hover away without being the visible label.
   */
  private mailDateEl(ts: number): HTMLElement {
    const el = document.createElement("time");
    el.dateTime = new Date(ts).toISOString();
    el.textContent = formatAbsolute(ts);
    el.title = formatRelative(ts);
    return el;
  }

  /**
   * The key an avatar's colour is derived from.
   *
   * An outgoing message is us, so it uses this vault's own protocol name. An
   * incoming one uses the contact's — the same key the chat rail uses, so the
   * same person is the same colour in both rails. A contact who has since been
   * removed falls back to the nickname, which is still stable per person.
   */
  private avatarKeyFor(m: MailMessage): string {
    if (m.outgoing) return this.status?.protocol_name ?? m.contactNick;
    return (
      this.contacts.find((c) => c.nickname === m.contactNick)?.protocol_name ??
      m.contactNick
    );
  }

  private mailAction(
    label: string,
    ic: IconName,
    onClick: () => void,
    active = false,
  ): HTMLElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `btn-icon-round pressable${active ? " active" : ""}`;
    b.appendChild(icon(ic));
    b.setAttribute("aria-label", label);
    b.title = label;
    b.onclick = onClick;
    return b;
  }

  private mailEmpty(text: string): HTMLElement {
    const empty = document.createElement("p");
    empty.className = "muted mail-empty";
    empty.textContent = text;
    return empty;
  }

  /**
   * Flip one folder flag.
   *
   * Folder membership is derived from these lists, so filing a message is a
   * prefs write rather than a rewrite of the encrypted log.
   */
  private async flagMail(
    flag: "mail_starred" | "mail_archived" | "mail_trashed",
    id: string,
    on: boolean,
  ): Promise<void> {
    if (!id) return;
    await setMailFlag(flag, id, on);
    // Filing the open message or drop away drops you back to the list it left.
    if (on && (flag === "mail_trashed" || flag === "mail_archived")) {
      this.mailThreadId = null;
      this.mailOpenId = null;
      this.dropInboxOpenId = null;
    }
    this.render();
  }

  /**
   * Remove a message from the encrypted log for good.
   *
   * The log is the archive, so this really does drop the record — but the
   * attachment blobs it pointed at are left in the vault, which has no
   * collection pass. That is the same trade chat already makes.
   */
  private async deleteMailForever(id: string): Promise<void> {
    if (!id) return;
    try {
      await api.deleteMail(id);
    } catch (e) {
      this.announce(e instanceof AppError ? e.message : "Could not delete.");
      return;
    }
    // Drop the id from the flag lists too, or they accumulate ids for records
    // that no longer exist.
    const flags = this.mailPrefs();
    const drop = (s: Set<string>) => [...s].filter((x) => x !== id);
    await patchUiPrefs({
      mail_read: drop(flags.read),
      mail_starred: drop(flags.starred),
      mail_archived: drop(flags.archived),
      mail_trashed: drop(flags.trashed),
    });
    this.mailThreadId = null;
    this.mailOpenId = null;
    this.dropInboxOpenId = null;
    await this.reloadMailbox();
    this.announce("Message deleted");
    this.render();
  }

  // ------------------------------------------------------ incoming drops

  private dropInboxSearch = "";

  /** Open a filed drop: show it quarantined, and mark it seen. */
  private openDrop(id: string): void {
    this.dropInboxOpenId = id;
    this.render();
    // Look the drop up in the mail drop store, not the Tor Drops directory:
    // same id shape, different records, and the registry lookup would always
    // miss and skip the read flag.
    const drop = getDropById(id);
    if (!drop) return;
    const flags = this.mailPrefs();
    if (flags.read.has(id)) return;
    const read = new Set(flags.read);
    read.add(id);
    void patchUiPrefs({ mail_read: [...read] }).then(() => {
      if (this.dropInboxOpenId === id) this.render();
    });
  }

  /** The Drops folder's list: one row per filed drop, newest first. */
  private renderDropList(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";

    const bar = document.createElement("div");
    bar.className = "mail-toolbar";
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = MAIL_FOLDER_LABEL.drops;
    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const inp = document.createElement("input");
    inp.placeholder = "Search drops";
    inp.value = this.dropInboxSearch;
    inp.oninput = () => {
      this.dropInboxSearch = inp.value;
      this.render();
    };
    search.appendChild(inp);
    bar.append(title, search);
    pane.appendChild(bar);

    const flags = this.mailPrefs();
    const needle = this.dropInboxSearch.trim().toLowerCase();
    const rows = listDropInbox(flags).filter(
      (d) =>
        !needle ||
        (d.subject ?? "").toLowerCase().includes(needle) ||
        d.body.toLowerCase().includes(needle),
    );

    if (!rows.length) {
      const empty = document.createElement("div");
      empty.className = "mail-empty";
      if (!needle && this.dropInboxKey && !this.dropInboxKey.exists) {
        // The one thing a fresh vault needs is the key — offer it here rather
        // than sending the user hunting through Settings first.
        empty.textContent =
          "No drop key yet — generate one, publish it anywhere, and drops sealed to it land here.";
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "btn-primary pressable";
        btn.textContent = "Generate drop key";
        btn.onclick = () => {
          btn.disabled = true;
          void api
            .dropsKeyCreate()
            .then((k) => {
              this.dropInboxKey = k;
              this.render();
            })
            .catch((e) => {
              this.announce(
                e instanceof AppError ? e.message : "Could not generate the drop key.",
              );
              this.render();
            });
        };
        empty.appendChild(btn);
      } else {
        empty.textContent = MAIL_EMPTY.drops;
      }
      pane.appendChild(empty);
      return pane;
    }

    const list = document.createElement("div");
    list.className = "mail-list";
    for (const d of rows) {
      list.appendChild(this.dropRow(d, flags, this.dropInboxOpenId === d.id));
    }
    pane.appendChild(list);
    return pane;
  }

  /**
   * One drop in a message list — the same shape as `mailRow`, with "Anonymous"
   * in the who column and the snippet showing, because a subject alone says
   * almost nothing for a drop. `active` marks the drop open in the Drops
   * folder; a drop listed under Trash or Archive is never the open one.
   */
  private dropRow(d: DropMessage, flags: MailFlags, active = false): HTMLElement {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `mail-row pressable${active ? " active" : ""}`;
    if (isDropUnread(d, flags)) row.classList.add("unread");

    const dot = document.createElement("span");
    dot.className = "mail-row-dot";

    // Drops have no sender, so the who column is always "Anonymous".
    const who = document.createElement("span");
    who.className = "mail-row-who";
    who.textContent = "Anonymous";

    const main = document.createElement("div");
    main.className = "mail-row-main";
    const subject = document.createElement("div");
    subject.className = "mail-row-subject";
    subject.textContent = d.subject || "(no subject)";
    const snip = document.createElement("div");
    snip.className = "mail-row-snippet";
    snip.textContent = previewText(d.body, d.attachments.length);
    main.append(subject, snip);

    const right = document.createElement("div");
    right.className = "mail-row-right";
    const time = document.createElement("span");
    time.className = "text-12 faint";
    time.textContent = formatRelative(d.receivedAt);
    right.appendChild(time);
    if (flags.starred.has(d.id)) right.appendChild(this.iconSmall("star"));
    if (d.attachments.length) right.appendChild(this.iconSmall("attach"));

    row.append(dot, who, main, right);
    row.onclick = () => this.openDrop(d.id);
    return row;
  }

  /**
   * The quarantined reading view for one filed drop.
   *
   * Every difference from `renderMailThread` here is a hardening decision:
   * the body is escaped plain text (never `bodyToDisplayHtml`), links are not
   * made clickable, attachments are save-only chips carrying their SHA-256,
   * and there is no Reply — the sender does not exist to reply to.
   */
  private renderDropView(id: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    const drop = getDropById(id);
    if (!drop) {
      // Filed away by a refresh? Back to the list rather than a dead pane.
      this.dropInboxOpenId = null;
      return this.renderDropList();
    }

    const head = document.createElement("div");
    head.className = "mail-read-head";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-icon-round pressable";
    back.appendChild(icon("back"));
    back.setAttribute("aria-label", "Back to Drops");
    back.onclick = () => {
      this.dropInboxOpenId = null;
      this.render();
    };
    const titles = document.createElement("div");
    titles.className = "mail-read-titles";
    const h = document.createElement("h1");
    h.className = "text-17";
    h.textContent = drop.subject || "(no subject)";
    titles.appendChild(h);

    // The same actions a mail thread has, minus Reply — there is no sender to
    // reply to. Filing (star, archive, trash) rides the same per-record flag
    // sets mail uses, and trash wins over everything, exactly as `inFolder`
    // decides it for mail.
    const flags = this.mailPrefs();
    const actions = document.createElement("div");
    actions.className = "mail-read-actions";
    if (flags.trashed.has(drop.id)) {
      actions.append(
        this.mailAction("Restore to Drops", "inbox", () =>
          void this.flagMail("mail_trashed", drop.id, false),
        ),
        this.mailAction("Delete forever", "trash", () =>
          void this.deleteMailForever(drop.id),
        ),
      );
    } else {
      const starred = flags.starred.has(drop.id);
      actions.append(
        this.mailAction(starred ? "Unstar" : "Star", "star", () =>
          void this.flagMail("mail_starred", drop.id, !starred), starred),
        this.mailAction("Archive", "archive", () =>
          void this.flagMail("mail_archived", drop.id, true),
        ),
        this.mailAction("Move to Trash", "trash", () =>
          void this.flagMail("mail_trashed", drop.id, true),
        ),
      );
    }
    actions.appendChild(
      this.mailAction("Forward", "send", () => this.forwardDrop(drop.id)),
    );
    head.append(back, titles, actions);
    pane.appendChild(head);

    // Provenance, in the reader's face: this came from outside Hush, with no
    // sender, and nothing here can be replied to or trusted like contact mail.
    const banner = document.createElement("div");
    banner.className = "drop-banner";
    banner.append(icon("alert"));
    const bannerText = document.createElement("div");
    bannerText.className = "text-13";
    bannerText.textContent =
      "From outside Hush — an anonymous sender via your drop key. This cannot be replied to, and the sender is not a verified contact. Open attachments with care.";
    banner.appendChild(bannerText);
    pane.appendChild(banner);

    const card = document.createElement("article");
    card.className = "mail-message";

    // The same two-column shape as `renderMailThread`: a fixed avatar column
    // and a content column. A drop has no sender, so the avatar is a neutral
    // mark rather than a contact colour — but the column still has to exist,
    // or the grid would drop the header and body into the 44px avatar slot and
    // wrap the body to a few characters per line.
    card.appendChild(createAvatarEl("anonymous-drop", "sm"));

    const content = document.createElement("div");
    content.className = "mail-message-content";

    const msgHead = document.createElement("dl");
    msgHead.className = "mail-message-head";
    msgHead.append(
      ...this.mailHeaderField("From", "Anonymous (via drop)"),
      ...this.mailHeaderField("To", this.mailAddress() ?? "you"),
      ...this.mailHeaderField("Received", this.mailDateEl(drop.receivedAt)),
      ...this.mailHeaderField("Sealed", this.mailDateEl(drop.createdAt)),
      ...this.mailHeaderField("Subject", drop.subject || "(no subject)"),
    );

    const rule = document.createElement("hr");
    rule.className = "mail-message-rule";

    const body = document.createElement("div");
    body.className = "text-15 mail-message-body drop-body";
    // textContent, never innerHTML: a drop body is untrusted text, so it is
    // shown as-is, unwrapped by any sanitizer, with links left inert.
    body.textContent = drop.body;

    content.append(msgHead, rule, body);

    if (drop.attachments.length) {
      const attachRule = document.createElement("hr");
      attachRule.className = "mail-message-rule";
      content.appendChild(attachRule);
      for (const a of drop.attachments) {
        content.appendChild(this.dropAttachmentChip(a));
      }
    }
    card.appendChild(content);
    pane.appendChild(card);
    return pane;
  }

  /**
  * Forward a filed drop to a contact. The quoted text is built with DOM text
  * nodes — a drop body is untrusted, so it is escaped by construction and
  * never passes through `innerHTML` — and the attachments ride along, since
  * their bytes are already in the vault.
  */
  private forwardDrop(id: string): void {
    const drop = getDropById(id);
    if (!drop) return;
    const quote = document.createElement("div");
    const rule = document.createElement("p");
    rule.textContent = "---------- Forwarded drop ----------";
    const meta = document.createElement("p");
    meta.append(
      document.createTextNode("From: Anonymous (via drop)"),
      document.createElement("br"),
      document.createTextNode(
        `Received: ${formatAbsolute(drop.receivedAt)}`,
      ),
    );
    const bodyP = document.createElement("p");
    const lines = drop.body.split("\n");
    lines.forEach((line, i) => {
      bodyP.append(document.createTextNode(line));
      if (i < lines.length - 1) bodyP.appendChild(document.createElement("br"));
    });
    quote.append(rule, meta, bodyP);
    this.openCompose({
      subject: fwdSubject(drop.subject || ""),
      body: quote.innerHTML,
      attachments: drop.attachments.length ? drop.attachments : undefined,
    });
  }

  /**
  * A drop's attachment: save-only, with the SHA-256 shown before you save.
   *
   * The blob id *is* the SHA-256 of the bytes — the vault keys blobs by it —
   * so displaying it costs nothing and is exactly the hash a careful recipient
   * checks after opening the file somewhere disposable. No preview is offered:
   * a drop's attachment is untrusted input, not media.
   */
  private dropAttachmentChip(a: MailAttachmentRef): HTMLElement {
    const chip = document.createElement("div");
    chip.className = "mail-attachment drop-attachment";
    const line = document.createElement("div");
    line.className = "drop-attachment-line";
    const name = document.createElement("span");
    name.className = "text-13";
    name.textContent = a.name;
    const size = document.createElement("span");
    size.className = "text-12 faint";
    size.textContent = formatBytes(a.size);
    line.append(name, size);
    const hash = document.createElement("code");
    hash.className = "text-11 faint drop-attachment-hash";
    hash.textContent = `SHA-256 ${a.blobId}`;
    hash.title = "The attachment's SHA-256, as saved in the vault";
    chip.append(line, hash);
    attachFileActions(chip, a.name, () => resolveMessageFileBytes(a), a.blobId);
    return chip;
  }

  private mailAttachmentChip(a: MailAttachmentRef): HTMLElement {
    const chip = document.createElement("div");
    chip.className = "mail-attachment";
    const name = document.createElement("span");
    name.className = "text-13";
    name.textContent = a.name;
    const size = document.createElement("span");
    size.className = "text-12 faint";
    size.textContent = formatBytes(a.size);
    chip.append(name, size);
    attachFileActions(chip, a.name, () => resolveMessageFileBytes(a), a.blobId);
    return chip;
  }

  /**
   * Make a rendered rich body's links safe to click.
   *
   * The sanitizer allows `href` on `http(s)`/`mailto` links, but this app has no
   * external opener wired up, so a raw click would either do nothing or navigate
   * the webview away from the app. Intercepting the click and copying the URL
   * keeps the link useful without handing the shell a navigation it cannot
   * safely perform.
   */
  private guardLinks(root: HTMLElement): void {
    root.addEventListener("click", (e) => {
      const link = (e.target as HTMLElement).closest?.("a[href]");
      if (!link) return;
      e.preventDefault();
      const href = link.getAttribute("href") ?? "";
      void navigator.clipboard
        .writeText(href)
        .then(() => this.announce("Link copied to clipboard"))
        .catch(() => this.announce(href));
    });
  }

  // ------------------------------------------------------------- compose

  /**
   * Forward one mail message to a contact. The quote reuses the stored body —
   * the composer sanitized it on send, and `bodyToDisplayHtml` re-sanitizes it
   * here — and the attachments ride along, their bytes loaded back from the
   * vault the same way a draft's are.
   */
  private forwardMail(m: MailMessage): void {
    const quote = document.createElement("div");
    const rule = document.createElement("p");
    rule.textContent = "---------- Forwarded message ----------";
    const meta = document.createElement("p");
    meta.append(
      document.createTextNode(
        `From: ${m.outgoing ? (this.mailAddress() ?? "you") : displayNick(m.contactNick)}`,
      ),
      document.createElement("br"),
      document.createTextNode(`Date: ${formatAbsolute(m.createdAt)}`),
      document.createElement("br"),
      document.createTextNode(`Subject: ${m.subject || "(no subject)"}`),
    );
    const bodyDiv = document.createElement("div");
    bodyDiv.innerHTML = bodyToDisplayHtml(m.body);
    quote.append(rule, meta, bodyDiv);
    this.openCompose({
      subject: fwdSubject(m.subject || threadSubject(m.threadId)),
      body: quote.innerHTML,
      attachments: m.attachments.length ? m.attachments : undefined,
    });
  }

  /** Open the full-pane composer for a new message, a reply, a forward, or a
   *  draft. */
  private openCompose(opts: {
    draftId?: string;
    to?: string;
    threadId?: string;
    subject?: string;
    /** Pre-seeded body, already safe HTML — a forward's quoted text. */
    body?: string;
    /** Attachment refs to carry over, with bytes loaded back from the vault. */
    attachments?: MailAttachmentRef[];
  }): void {
    this.mailComposeOpen = true;
    this.mailDraftId = opts.draftId ?? null;
    this.mailReplyTo = opts.to ?? null;
    this.mailReplyThread = opts.threadId ?? null;
    this.mailSubjectSeed = opts.subject ?? null;
    this.mailComposeFiles = [];
    // A brand-new message has no conversation to return to. Without this,
    // opening compose from a thread and then saving a draft or pressing Back
    // lands on the folder list while the reading pane still shows the thread
    // that happened to be open.
    if (!opts.draftId && !opts.threadId) {
      this.mailThreadId = null;
      this.mailOpenId = null;
    }
    // Seed the editable fields from the draft (or the reply) before rendering,
    // so the first paint already has the right values and a later re-render
    // keeps them.
    const draft = opts.draftId ? draftById(opts.draftId) : undefined;
    this.mailComposeTo = opts.to ?? draft?.to ?? "";
    this.mailComposeSubject = draft?.subject ?? opts.subject ?? "";
    this.mailComposeBody = opts.body ?? draft?.body ?? "";
    this.render();

    // Editing a draft — or forwarding — carries attachments over, and the log
    // only kept blob ids, so the bytes come back out of the vault.
    const refs = draft?.attachments.length
      ? draft.attachments
      : opts.attachments;
    if (refs?.length) {
      void this.loadDraftAttachments(refs);
    }
  }

  private async loadDraftAttachments(refs: MailAttachmentRef[]): Promise<void> {
    const loaded: { name: string; dataBase64: string; size: number }[] = [];
    for (const a of refs) {
      try {
        const r = await api.blob(a.blobId);
        if (r?.data_base64) {
          loaded.push({ name: a.name, dataBase64: r.data_base64, size: a.size });
        }
      } catch {
        this.announce(`Could not load ${a.name} from the vault`);
      }
    }
    // The composer may have been closed while those fetches were in flight.
    if (!this.mailComposeOpen) return;
    this.mailComposeFiles = loaded;
    this.render();
  }

  private closeCompose(): void {
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.mailReplyTo = null;
    this.mailReplyThread = null;
    this.mailSubjectSeed = null;
    this.mailComposeFiles = [];
    this.mailComposeTo = "";
    this.mailComposeSubject = "";
    this.mailComposeBody = "";
    this.render();
  }

  private renderMailCompose(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    const draft = this.mailDraftId ? draftById(this.mailDraftId) : undefined;

    const head = document.createElement("div");
    head.className = "mail-toolbar";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-icon-round pressable";
    back.appendChild(icon("back"));
    back.setAttribute("aria-label", "Back to mail");
    back.onclick = () => this.closeCompose();
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = draft
      ? "Edit draft"
      : this.mailReplyThread
        ? "Reply"
        : "New mail";
    head.append(back, title);
    pane.appendChild(head);

    const form = document.createElement("div");
    form.className = "mail-compose";

    const isReply = !!this.mailReplyThread;

    // A keyboard-typeable picker: type to narrow, arrow to move, Enter to
    // choose. It reads and writes `this.mailComposeTo`, which is held outside
    // the DOM so that rebuilding this pane (an attachment load) cannot forget
    // the recipient.
    const toPicker = new ContactPicker({
      items: this.contacts.map((c) => ({
        nickname: c.nickname,
        label: c.mail_handle
          ? `${displayNick(c.nickname)} (${this.contactAddress(c)})`
          : displayNick(c.nickname),
        // The raw nickname stays searchable even when a display-name override
        // hides it from the label.
        keywords: c.nickname,
      })),
      value: this.mailComposeTo,
      label: "To",
      placeholder: "Type a name…",
      // A reply goes back to the thread's contact; changing it would fork the
      // conversation, so the picker is fixed.
      locked: isReply,
      emptyMessage: "No contacts yet. Add someone in Trusts to mail them.",
      noMatchMessage: "No contacts match that name.",
      onChange: (nickname) => {
        this.mailComposeTo = nickname;
      },
    });

    form.appendChild(this.composeRow("To", toPicker.element));

    const subj = document.createElement("input");
    subj.className = "input-field mail-compose-subject";
    subj.placeholder = "(no subject)";
    subj.value = this.mailComposeSubject;
    subj.oninput = () => {
      this.mailComposeSubject = subj.value;
    };
    form.appendChild(this.composeRow("Subject", subj));

    // The body is rich text. `onChange` mirrors each edit into the field above
    // so that a rebuild of this pane — attaching a file, or the async load of a
    // draft's attachments — does not throw away what was typed.
    const editor = new RichTextEditor({
      value: this.mailComposeBody,
      placeholder: "Write your message",
      label: "Message body",
      onChange: () => {
        this.mailComposeBody = editor.getHtml();
      },
    });
    form.appendChild(editor.element);

    const preview = document.createElement("div");
    preview.className = "mail-attachment-preview";

    const attach = document.createElement("button");
    attach.type = "button";
    attach.className = "btn-secondary text-13 pressable";
    attach.onclick = () => files.click();

    // Rebuilt in place rather than through `this.render()`, so adding or
    // removing a file leaves the caret in the body where it was.
    const renderChips = () => {
      preview.innerHTML = "";
      this.mailComposeFiles.forEach((f, i) => {
        const chip = document.createElement("div");
        chip.className = "mail-attachment";
        const name = document.createElement("span");
        name.className = "text-13";
        name.textContent = f.name;
        const size = document.createElement("span");
        size.className = "text-12 faint";
        size.textContent = formatBytes(f.size);
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "btn-icon-round pressable";
        remove.appendChild(this.iconSmall("close"));
        remove.setAttribute("aria-label", `Remove ${f.name}`);
        remove.onclick = () => {
          this.mailComposeFiles.splice(i, 1);
          renderChips();
        };
        chip.append(name, size, remove);
        preview.appendChild(chip);
      });
      attach.textContent = this.mailComposeFiles.length
        ? "Add files"
        : "Attach files";
    };
    renderChips();

    const files = document.createElement("input");
    files.type = "file";
    files.multiple = true;
    files.className = "mail-file-input";
    files.onchange = () => {
      void this.addComposeFiles(Array.from(files.files ?? []), renderChips);
      // Reset so picking the same file twice still fires a change event.
      files.value = "";
    };

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.hidden = true;

    const saveDraft = document.createElement("button");
    saveDraft.type = "button";
    saveDraft.className = "btn-secondary pressable";
    saveDraft.textContent = "Save draft";
    saveDraft.onclick = () =>
      void this.saveDraft(saveDraft, {
        to: toPicker.value,
        subject: subj.value,
        body: editor.getHtml(),
        err,
      });

    const send = document.createElement("button");
    send.type = "button";
    send.className = "btn-primary pressable";
    send.textContent = isReply ? "Send reply" : "Send";
    send.onclick = () => {
      void this.sendMail(send, {
        to: toPicker.value,
        subject: subj.value,
        body: editor.getHtml(),
        err,
      });
    };

    const actions = document.createElement("div");
    actions.className = "mail-compose-actions";
    actions.append(attach, saveDraft);
    // Only a saved draft can be discarded. A brand new message is thrown away
    // by closing compose, which is what Back already does.
    if (this.mailDraftId) {
      const discard = document.createElement("button");
      discard.type = "button";
      discard.className = "btn-secondary pressable";
      discard.textContent = "Discard draft";
      discard.onclick = () => void this.discardDraft(discard, err);
      actions.appendChild(discard);
    }
    actions.appendChild(send);

    form.append(files, preview, actions, err);
    pane.appendChild(form);
    return pane;
  }

  private composeRow(label: string, field: HTMLElement): HTMLElement {
    const row = document.createElement("div");
    row.className = "mail-compose-row";
    const l = document.createElement("span");
    l.className = "mail-compose-label";
    l.textContent = label;
    row.append(l, field);
    return row;
  }

  private async addComposeFiles(
    picked: File[],
    after: () => void,
  ): Promise<void> {
    const next = [...this.mailComposeFiles];
    for (const f of picked) {
      next.push({
        name: f.name,
        dataBase64: await fileToBase64(f),
        size: f.size,
      });
    }
    this.mailComposeFiles = next;
    after();
  }

  /** The client-side half of the daemon's caps, so the limit is explained
   *  before a round trip rather than after one. */
  private composeError(
    to: string,
    body: string,
  ): string | null {
    if (!to) return "Add a contact first.";
    return this.attachmentOrBodyError(body);
  }

  /** The checks a send shares: attachment caps, and not sending nothing. */
  private attachmentOrBodyError(body: string): string | null {
    if (this.mailComposeFiles.length > MAX_MAIL_ATTACHMENTS) {
      return `At most ${MAX_MAIL_ATTACHMENTS} attachments.`;
    }
    const total = this.mailComposeFiles.reduce((n, f) => n + f.size, 0);
    if (total > MAX_MAIL_BYTES) {
      return `Attachments are limited to ${Math.round(MAX_MAIL_BYTES / 1024)} KB in total.`;
    }
    // The body is HTML; an editor holding only an empty paragraph is not text.
    if (isBlankHtml(body) && !this.mailComposeFiles.length) {
      return "Write a message or attach a file.";
    }
    return null;
  }

  private async saveDraft(
    btn: HTMLButtonElement,
    input: { to: string; subject: string; body: string; err: HTMLElement },
  ): Promise<void> {
    input.err.hidden = true;
    // A draft with no recipient and no text is not worth keeping.
    if (
      !input.to &&
      isBlankHtml(input.body) &&
      !input.subject.trim() &&
      !this.mailComposeFiles.length
    ) {
      input.err.hidden = false;
      input.err.textContent = "Nothing to save yet.";
      return;
    }
    btn.disabled = true;
    try {
      await api.saveDraft({
        id: this.mailDraftId ?? undefined,
        thread_id: this.mailReplyThread ?? undefined,
        to: input.to,
        subject: input.subject.trim() || undefined,
        body: input.body,
        attachments: this.mailComposeFiles.map((f) => ({
          name: f.name,
          data_base64: f.dataBase64,
        })),
      });
      await this.reloadMailbox();
      this.mailFolder = "drafts";
      this.closeCompose();
      this.announce("Draft saved");
    } catch (e) {
      input.err.hidden = false;
      input.err.textContent =
        e instanceof AppError ? e.message : "Couldn't save the draft.";
    } finally {
      btn.disabled = false;
    }
  }

  /**
   * Throw the open draft away for good, dropping its record from the log.
   *
   * Only reachable from a saved draft; a new message is abandoned by leaving
   * compose, and nothing was written for it in the first place.
   */
  private async discardDraft(
    btn: HTMLButtonElement,
    err: HTMLElement,
  ): Promise<void> {
    const id = this.mailDraftId;
    if (!id) return;
    err.hidden = true;
    btn.disabled = true;
    try {
      await api.deleteDraft(id);
      await this.reloadMailbox();
      this.mailComposeOpen = false;
      this.mailDraftId = null;
      this.mailComposeFiles = [];
      this.announce("Draft discarded");
      this.render();
    } catch (e) {
      btn.disabled = false;
      err.hidden = false;
      err.textContent =
        e instanceof AppError ? e.message : "Couldn't discard the draft.";
    }
  }

  private async sendMail(
    btn: HTMLButtonElement,
    input: { to: string; subject: string; body: string; err: HTMLElement },
  ): Promise<void> {
    input.err.hidden = true;
    const problem = this.composeError(input.to, input.body);
    if (problem) {
      input.err.hidden = false;
      input.err.textContent = problem;
      return;
    }
    btn.disabled = true;
    const draftId = this.mailDraftId;
    const isReply = !!this.mailReplyThread;
    try {
      const resp = await api.sendMail(input.to, {
        thread_id: this.mailReplyThread ?? undefined,
        subject: input.subject.trim() || undefined,
        body: input.body,
        attachments: this.mailComposeFiles.map((f) => ({
          name: f.name,
          data_base64: f.dataBase64,
        })),
      });
      // Sending a draft consumes it, or the same message would sit in Drafts
      // forever after going out.
      if (draftId) {
        try {
          await api.deleteDraft(draftId);
        } catch {
          /* the mail is sent; a stale draft is not worth an error */
        }
      }
      await this.reloadMailbox();
      this.mailComposeOpen = false;
      this.mailDraftId = null;
      this.mailReplyTo = null;
      this.mailReplyThread = null;
      this.mailSubjectSeed = null;
      this.mailComposeFiles = [];
      // Show the message where it landed: Sent, or the Outbox if the relay
      // refused it — saying "sent" then filing it under Outbox would be a lie.
      this.mailFolder = resp?.queued ? "outbox" : "sent";
      this.mailThreadId = resp?.thread_id ?? null;
      this.mailOpenId = null;
      this.announce(
        resp?.queued
          ? "Queued — the relay is unreachable, it will send when it is back"
          : isReply
            ? "Reply sent"
            : "Mail sent",
      );
      this.render();
    } catch (e) {
      input.err.hidden = false;
      input.err.textContent =
        e instanceof AppError ? e.message : "Couldn't send. Try again.";
    } finally {
      btn.disabled = false;
    }
  }

  // ------------------------------------------------------------------
  // Torrents (TorBox-powered client)
  //
  // The pane is a thin view over `torrentsStore`, which polls the daemon. The
  // daemon owns the pipeline (TorBox → vault); nothing here talks to TorBox.
  // ------------------------------------------------------------------

  /** Open the wallet from the Vault tile: full-bleed, with a way back. */
  private walletOpenWallet(): void {
    this.walletOpen = true;
    this.notesOpen = false;
    this.render();
  }

  /** Leave the wallet for the Vault bento. The phrase card survives a trip out. */
  private walletBackToVault(): void {
    this.walletOpen = false;
    this.render();
  }

  /**
   * The Wallet pane, reached from the Vault bento: one self-custody wallet per
   * vault, on two chains.
   *
   * The phrase is generated and sealed by the daemon; this pane sees it exactly
   * once, at creation, and is responsible for making the user write it down.
   * Everything else — the balances, the addresses, sending — is the daemon's
   * state rendered back.
   *
   * The chain capsule switches which chain the cards below describe. One
   * phrase backs both, so there is no separate "create" per chain: the tabs
   * are a view, not two wallets.
   */
  private renderWallet(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane wallet-pane";
    const state = getWalletState();
    const view = chainState(state, this.walletChain);

    const head = document.createElement("div");
    head.className = `wallet-head${this.walletOpen ? " with-back" : ""}`;
    if (this.walletOpen) {
      const back = document.createElement("button");
      back.type = "button";
      back.className = "btn-secondary pressable wallet-back";
      back.textContent = "← Vault";
      back.addEventListener("click", () => this.walletBackToVault());
      head.appendChild(back);
    }
    const title = document.createElement("h1");
    title.className = "text-22";
    title.textContent = "Wallet";
    head.appendChild(title);
    if (state.exists) {
      const sync = document.createElement("button");
      sync.type = "button";
      sync.className = "btn-secondary text-13 pressable";
      sync.textContent = this.walletBusy
        ? "Working…"
        : this.walletChain === "bitcoin"
          ? "Sync"
          : "Refresh";
      sync.disabled = this.walletBusy || !view.available;
      sync.title =
        this.walletChain === "bitcoin"
          ? "Scan the chain for payments to this wallet"
          : "Read the balance and every tracked token from the node";
      sync.onclick = () => void this.walletSyncNow();
      head.appendChild(sync);
    }
    pane.appendChild(head);

    if (!isTauriShell()) {
      pane.appendChild(this.walletNeedsDesktop());
      return pane;
    }

    // Nothing has been read yet. Until the daemon answers, the pane cannot
    // tell "no wallet" from "not reachable", and showing the create form to
    // someone who already has a wallet would be worse than showing nothing.
    if (!isWalletLoaded()) {
      const wait = document.createElement("p");
      wait.className = "muted text-13";
      wait.textContent = getWalletLoadError() ?? "Reading the wallet…";
      pane.appendChild(wait);
      return pane;
    }

    if (this.walletError) {
      const warn = document.createElement("p");
      warn.className = "wallet-error text-13";
      warn.textContent = this.walletError;
      pane.appendChild(warn);
    }

    if (!state.exists) {
      pane.appendChild(this.walletSetup(state));
      return pane;
    }

    if (this.walletPhrase) {
      pane.appendChild(this.walletPhraseCard(this.walletPhrase));
    }
    pane.appendChild(this.walletChainCard(state));
    pane.appendChild(this.walletBalanceCard(view, state));
    pane.appendChild(this.walletReceiveCard(view));
    if (this.walletChain === "ethereum") {
      pane.appendChild(this.walletSendCard(view, state));
      pane.appendChild(this.walletTokensCard(state));
    } else {
      pane.appendChild(this.walletSendCard(view, state));
    }
    pane.appendChild(this.walletModeCard(state));
    pane.appendChild(this.walletDangerCard());
    return pane;
  }

  /**
   * The chain capsule: which chain the cards below describe.
   *
   * A chain this daemon was not built with is shown, but disabled and named as
   * unavailable — hiding it would leave a user wondering where their money is.
   */
  private walletChainCard(state: WalletStateDto): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Chains";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "One recovery phrase, both chains. Bitcoin and Ethereum are derived from the same twelve words.";
    card.append(h, p);

    const capsule = document.createElement("div");
    capsule.className = "segmented wallet-chains";
    capsule.setAttribute("role", "group");
    capsule.setAttribute("aria-label", "Wallet chain");
    const labels: { id: WalletChain; label: string }[] = [
      { id: "bitcoin", label: "Bitcoin" },
      { id: "ethereum", label: "Ethereum" },
    ];
    for (const opt of labels) {
      const chain = chainState(state, opt.id);
      const active = this.walletChain === opt.id;
      const b = document.createElement("button");
      b.type = "button";
      b.className = `pressable${active ? " active" : ""}`;
      b.textContent = opt.label;
      b.disabled = !chain.available;
      b.setAttribute("aria-pressed", active ? "true" : "false");
      if (!chain.available) {
        b.title = "This build of Hush has no adapter for this chain.";
      }
      b.onclick = () => {
        if (active || !chain.available) return;
        this.walletChain = opt.id;
        this.walletError = null;
        this.render();
      };
      capsule.appendChild(b);
    }
    card.appendChild(capsule);
    return card;
  }

  /** The wallet routes live on the daemon's Unix socket only (DR-02). */
  private walletNeedsDesktop(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "wallet-empty";
    const ic = icon("wallet");
    ic.classList.add("wallet-empty-icon");
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "The wallet lives in the desktop app";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "The wallet can move money, so the daemon only serves it over the local socket the desktop app uses — never over the TCP port a browser can reach. Open Hush to use it.";
    wrap.append(ic, h, p);
    return wrap;
  }

  /** No wallet yet: create one, or restore from a phrase. */
  private walletSetup(state: WalletStateDto): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "wallet-setup";

    const intro = document.createElement("p");
    intro.className = "muted text-13";
    intro.textContent =
      "Hush holds the keys, not a company. The recovery phrase is sealed into this vault with your master key and never leaves it — but a wallet you cannot restore from a phrase is a wallet you cannot recover, so write it down when it is shown. One phrase covers Bitcoin and Ethereum.";

    const createCard = document.createElement("section");
    createCard.className = "wallet-card";
    createCard.innerHTML = `<h2 class="text-18">Create a wallet</h2><p class="muted text-13">A new Bitcoin and Ethereum wallet, derived from one fresh 12-word phrase.</p>`;
    const createBtn = document.createElement("button");
    createBtn.type = "button";
    createBtn.className = "btn-primary pressable";
    createBtn.textContent = this.walletBusy ? "Working…" : "Create wallet";
    createBtn.disabled = this.walletBusy;
    createBtn.onclick = () => void this.walletCreateNow();
    createCard.appendChild(createBtn);

    const restoreCard = document.createElement("section");
    restoreCard.className = "wallet-card";
    restoreCard.innerHTML = `<h2 class="text-18">Restore from a phrase</h2><p class="muted text-13">Bring back a wallet you already hold the phrase for, on both chains. Nothing is overwritten — a vault holds one wallet.</p>`;
    const input = document.createElement("textarea");
    input.className = "input-field wallet-phrase-input";
    input.rows = 3;
    input.placeholder = "twelve words, separated by spaces";
    input.spellcheck = false;
    input.autocapitalize = "off";
    input.value = this.walletRestoreInput;
    input.oninput = () => {
      this.walletRestoreInput = input.value;
    };
    const restoreBtn = document.createElement("button");
    restoreBtn.type = "button";
    restoreBtn.className = "btn-secondary pressable";
    restoreBtn.textContent = this.walletBusy ? "Working…" : "Restore wallet";
    restoreBtn.disabled = this.walletBusy;
    restoreBtn.onclick = () => void this.walletRestoreNow();
    restoreCard.append(input, restoreBtn);

    // The posture is chosen here, before the wallet exists, because a Tor
    // wallet cannot be created until the shell's Tor is listening. Creating
    // first and offering the switch afterwards would mean either a clearnet
    // wallet nobody asked for or a create that fails after the fact.
    wrap.append(intro, this.walletModeCard(state), createCard, restoreCard);
    return wrap;
  }

  /** The one and only time the phrase is shown. */
  private walletPhraseCard(phrase: string): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card wallet-phrase-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Write this down now";
    const p = document.createElement("p");
    p.className = "text-13";
    p.textContent =
      "These twelve words are both wallets — Bitcoin and Ethereum. Anyone who has them can spend the money, and nobody — including Hush — can restore them for you. This is the only time they are shown.";
    const words = document.createElement("ol");
    words.className = "wallet-phrase-words";
    for (const w of phrase.split(/\s+/)) {
      const li = document.createElement("li");
      li.textContent = w;
      words.appendChild(li);
    }
    const actions = document.createElement("div");
    actions.className = "wallet-card-actions";
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "btn-secondary pressable";
    copy.textContent = "Copy phrase";
    copy.onclick = () => {
      void navigator.clipboard.writeText(phrase);
      showToast("Phrase copied. Paste it somewhere offline.", { kind: "success" });
    };
    const done = document.createElement("button");
    done.type = "button";
    done.className = "btn-primary pressable";
    done.textContent = "I have written it down";
    done.onclick = () => {
      this.walletPhrase = null;
      this.render();
    };
    actions.append(copy, done);
    card.append(h, p, words, actions);
    return card;
  }

  /** The balance of the chain on screen, in its own unit. */
  private walletBalanceCard(
    view: WalletChainDto,
    state: WalletStateDto,
  ): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = this.walletChain === "bitcoin" ? "Bitcoin balance" : "Ether balance";
    const amount = document.createElement("p");
    amount.className = "wallet-balance";
    amount.textContent =
      this.walletChain === "bitcoin"
        ? `${formatBtc(view.confirmed)} BTC`
        : `${formatEth(view.confirmed)} ETH`;
    const base = document.createElement("p");
    base.className = "faint text-12";
    base.textContent = formatBase(view.confirmed, view.unit);
    card.append(h, amount, base);
    if (view.pending !== "0") {
      const pending = document.createElement("p");
      pending.className = "text-13";
      pending.style.color = "var(--warning-text)";
      pending.textContent = `Waiting to confirm: ${formatBase(view.pending, view.unit)}`;
      card.appendChild(pending);
    }
    if (this.walletChain === "ethereum" && state.tokens.length > 0) {
      const list = document.createElement("ul");
      list.className = "wallet-token-list";
      for (const token of state.tokens) {
        const li = document.createElement("li");
        li.className = "wallet-token-row";
        const symbol = document.createElement("span");
        symbol.className = "wallet-token-symbol";
        symbol.textContent = token.symbol;
        const value = document.createElement("span");
        value.className = "wallet-token-balance";
        value.textContent = formatToken(token.balance, token.decimals, "");
        li.append(symbol, value);
        list.appendChild(li);
      }
      card.appendChild(list);
    }
    const note = document.createElement("p");
    note.className = "faint text-12";
    note.textContent =
      this.walletChain === "bitcoin"
        ? "A balance only moves when the wallet scans the chain. Press Sync after you expect a payment."
        : "A balance is read from the node, and is only as fresh as the last refresh.";
    card.appendChild(note);
    return card;
  }

  private walletReceiveCard(view: WalletChainDto): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Receive";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      this.walletChain === "bitcoin"
        ? "Share this address to be paid. It is the next unused one, so it changes after a payment arrives."
        : "This is the wallet's Ethereum account. Unlike Bitcoin it does not change — every payment to it lands in the same place.";
    card.append(h, p);

    const address = view.address;
    if (!address) {
      const none = document.createElement("p");
      none.className = "faint text-13";
      none.textContent = "No address yet — sync the wallet to derive one.";
      card.appendChild(none);
      return card;
    }

    const box = document.createElement("div");
    box.className = "wallet-address";
    const code = document.createElement("code");
    code.textContent = address;
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "btn-icon-round pressable";
    copy.setAttribute("aria-label", "Copy address");
    copy.title = "Copy address";
    copy.appendChild(icon("copy"));
    copy.onclick = () => {
      void navigator.clipboard.writeText(address);
      showToast("Address copied.", { kind: "success" });
    };
    box.append(code, copy);
    card.appendChild(box);

    const qr = document.createElement("canvas");
    qr.className = "wallet-qr";
    qr.setAttribute("aria-label", `QR code for ${address}`);
    void QRCode.toCanvas(qr, address, { width: 148, margin: 1 }).catch(() => {
      /* a QR that will not render is not worth an error over */
    });
    card.appendChild(qr);

    if (this.walletChain === "bitcoin") {
      const fresh = document.createElement("button");
      fresh.type = "button";
      fresh.className = "btn-secondary text-13 pressable";
      fresh.textContent = "New address";
      fresh.disabled = this.walletBusy;
      fresh.onclick = () => void this.walletNewAddress();
      card.appendChild(fresh);
    }
    return card;
  }

  /**
   * The send form, in the unit of whatever is being sent.
   *
   * Bitcoin takes satoshi, as it always has. Ethereum takes ether or the
   * token's own units, because typing wei is not a thing anyone can do: one
   * ether is 1e18 of them. The conversion to base units is exact integer
   * arithmetic, and a value the unit cannot represent is refused rather than
   * rounded away.
   */
  private walletSendCard(view: WalletChainDto, state: WalletStateDto): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Send";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      this.walletChain === "bitcoin"
        ? "Bitcoin only, on the wallet's own network. The fee is what the miners take."
        : "Ether, or one of the tokens this wallet tracks. The fee is set by the node's own estimate.";
    card.append(h, p);

    const to = document.createElement("input");
    to.className = "input-field";
    to.placeholder =
      this.walletChain === "bitcoin" ? "Destination address" : "Destination 0x address";
    to.spellcheck = false;
    to.value = this.walletSendTo;
    to.oninput = () => {
      this.walletSendTo = to.value;
    };

    // The token selector comes before the amount: the amount's unit is
    // whatever the selector says, so it has to be known first.
    let token: WalletTokenDto | null = null;
    if (this.walletChain === "ethereum") {
      const picker = document.createElement("select");
      picker.className = "input-field wallet-token-picker";
      picker.setAttribute("aria-label", "What to send");
      const native = document.createElement("option");
      native.value = "";
      native.textContent = "Ether (ETH)";
      picker.appendChild(native);
      for (const t of state.tokens) {
        const opt = document.createElement("option");
        opt.value = t.address;
        opt.textContent = `${t.symbol} — ${formatUnits(t.balance, t.decimals)}`;
        picker.appendChild(opt);
      }
      token =
        state.tokens.find((t) => t.address === this.walletSendToken) ?? null;
      picker.value = token?.address ?? "";
      picker.onchange = () => {
        this.walletSendToken = picker.value || null;
        // The amount's unit just changed, so what is typed is no longer the
        // amount the user meant.
        this.walletSendAmount = "";
        this.render();
      };
      card.appendChild(picker);
    }

    const unit = token ? token.symbol : this.walletChain === "bitcoin" ? "sat" : "ETH";
    const decimals = token ? token.decimals : this.walletChain === "bitcoin" ? 0 : 18;
    const amount = document.createElement("input");
    amount.className = "input-field";
    amount.placeholder = `Amount (${unit})`;
    amount.inputMode = "decimal";
    amount.value = this.walletSendAmount;
    const hint = document.createElement("p");
    hint.className = "faint text-12";
    const describe = (): string => {
      const base = parseUnits(this.walletSendAmount, decimals);
      if (base === null) {
        return this.walletSendAmount.trim() === ""
          ? `Amount in ${unit}.`
          : `That is more precision than ${unit} has.`;
      }
      if (this.walletChain === "bitcoin") {
        return `= ${formatBtc(base)} BTC`;
      }
      if (token) {
        return `= ${formatBase(base, `of the smallest ${token.symbol} unit`)}`;
      }
      return `= ${formatBase(base, "wei")}`;
    };
    amount.oninput = () => {
      this.walletSendAmount = amount.value;
      hint.textContent = describe();
    };
    hint.textContent = describe();

    const send = document.createElement("button");
    send.type = "button";
    send.className = "btn-primary pressable";
    send.textContent = this.walletBusy ? "Sending…" : "Send";
    send.disabled = this.walletBusy;
    send.onclick = () => void this.walletSendNow();

    card.append(to, amount, hint);
    if (this.walletChain === "bitcoin") {
      const fee = document.createElement("input");
      fee.className = "input-field";
      fee.placeholder = "Fee rate (sat/vB)";
      fee.inputMode = "numeric";
      fee.value = this.walletSendFee;
      fee.oninput = () => {
        this.walletSendFee = fee.value;
      };
      card.appendChild(fee);
    }
    card.appendChild(send);

    if (this.walletLastTx && this.walletLastTx.chain === this.walletChain) {
      const done = document.createElement("p");
      done.className = "text-13";
      done.style.color = "var(--success-text)";
      done.textContent = this.walletLastTx.fee_sat
        ? `Sent. Fee ${formatBase(String(this.walletLastTx.fee_sat), "sat")}.`
        : "Sent. The fee is whatever the transaction was mined with.";
      const txid = document.createElement("code");
      txid.className = "wallet-txid";
      txid.textContent = this.walletLastTx.txid;
      card.append(done, txid);
    }

    const hasFunds =
      view.confirmed !== "0" ||
      (this.walletChain === "ethereum" &&
        state.tokens.some((t) => t.balance !== "0"));
    if (!hasFunds) {
      const none = document.createElement("p");
      none.className = "faint text-12";
      none.textContent =
        "There is nothing to send until the wallet holds a confirmed balance.";
      card.appendChild(none);
    }
    return card;
  }

  /**
   * The ERC-20 tokens this wallet tracks.
   *
   * Adding a token reads its symbol and decimals from the contract, so the
   * wallet never takes the user's word for how many decimals a token has —
   * that number is what turns a balance into a displayed amount.
   */
  private walletTokensCard(state: WalletStateDto): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Tokens";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "ERC-20 tokens to watch. Hush reads the symbol and decimals from the contract, so a wrong number is never trusted.";
    card.append(h, p);

    if (state.tokens.length === 0) {
      const none = document.createElement("p");
      none.className = "faint text-13";
      none.textContent = "No tokens yet. Add one by its contract address.";
      card.appendChild(none);
    }

    for (const token of state.tokens) {
      const row = document.createElement("div");
      row.className = "wallet-token-row";
      const info = document.createElement("div");
      info.className = "wallet-token-info";
      const symbol = document.createElement("span");
      symbol.className = "wallet-token-symbol";
      symbol.textContent = token.symbol;
      const balance = document.createElement("span");
      balance.className = "wallet-token-balance";
      balance.textContent = formatUnits(token.balance, token.decimals);
      const address = document.createElement("code");
      address.className = "wallet-token-address";
      address.textContent = token.address;
      info.append(symbol, balance, address);
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "btn-secondary text-13 pressable";
      remove.textContent = "Remove";
      remove.disabled = this.walletBusy;
      remove.onclick = () => void this.walletRemoveToken(token.address);
      row.append(info, remove);
      card.appendChild(row);
    }

    const input = document.createElement("input");
    input.className = "input-field";
    input.placeholder = "Token contract address (0x…)";
    input.spellcheck = false;
    input.value = this.walletTokenInput;
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-secondary pressable";
    add.textContent = this.walletBusy ? "Reading…" : "Add token";
    add.disabled = this.walletBusy || !this.walletTokenInput.trim();
    add.onclick = () => void this.walletAddToken();
    input.oninput = () => {
      this.walletTokenInput = input.value;
      // The button is enabled by having an address, so its state has to follow
      // the field. Patched in place rather than re-rendered: a full render
      // would drop the caret out of the field mid-address.
      add.disabled = this.walletBusy || !input.value.trim();
    };
    card.append(input, add);
    return card;
  }

  /**
   * The clearnet/Tor capsule.
   *
   * The rule the daemon enforces and this mirrors: while Hush's relay is on Tor
   * the wallet is on Tor and clearnet is not offered. On clearnet the user may
   * choose, and the choice sticks until the relay moves. It applies to both
   * chains at once: one wallet, one network posture.
   */
  private walletModeCard(state: WalletStateDto): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Network";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent = state.mode_locked
      ? "Hush is routing through Tor, so the wallet is too. It is never less private than the rest of the app."
      : "Clearnet reaches the chain source directly, so it sees your IP. Tor hides it. Your balance and addresses are the same either way.";
    card.append(h, p);

    const capsule = document.createElement("div");
    capsule.className = "segmented wallet-mode";
    capsule.setAttribute("role", "group");
    capsule.setAttribute("aria-label", "Wallet network");
    const options: { id: WalletNetworkMode; label: string }[] = [
      { id: "clearnet", label: "Clearnet" },
      { id: "tor", label: "Tor" },
    ];
    for (const opt of options) {
      const active = state.mode === opt.id;
      const locked = state.mode_locked && opt.id !== "tor";
      const b = document.createElement("button");
      b.type = "button";
      b.className = `pressable${active ? " active" : ""}`;
      b.textContent = opt.label;
      b.disabled = locked || this.walletBusy;
      b.setAttribute("aria-pressed", active ? "true" : "false");
      if (locked) b.title = "The relay is on Tor, so the wallet stays on Tor.";
      b.onclick = () => {
        if (active || locked) return;
        void this.walletChangeMode(opt.id);
      };
      capsule.appendChild(b);
    }
    card.appendChild(capsule);

    if (state.mode_locked) {
      const lock = document.createElement("p");
      lock.className = "faint text-12";
      lock.textContent = "Locked on Tor while the relay uses it.";
      card.appendChild(lock);
    }
    return card;
  }

  private walletDangerCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "wallet-card wallet-danger";
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Remove this wallet";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "Erases the phrase and the wallet state from this vault. The coins stay on the chain — but without the phrase they are gone for good. Type remove to confirm.";
    const input = document.createElement("input");
    input.className = "input-field";
    input.placeholder = "remove";
    input.value = this.walletWipeConfirm;
    input.oninput = () => {
      this.walletWipeConfirm = input.value;
    };
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn-danger pressable";
    btn.textContent = "Remove wallet";
    btn.disabled = this.walletBusy || this.walletWipeConfirm.trim() !== "remove";
    btn.onclick = () => void this.walletWipeNow();
    card.append(h, p, input, btn);
    return card;
  }

  // --- Wallet actions -------------------------------------------------------

  private async walletCreateNow(): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      const mode = getWalletState().mode;
      if (mode === "tor") await this.ensureWalletTor();
      this.walletPhrase = await createWallet(
        this.walletNetwork,
        DEFAULT_ETHEREUM_NETWORK,
        mode,
      );
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "The wallet could not be created.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletRestoreNow(): Promise<void> {
    const phrase = this.walletRestoreInput.trim();
    if (!phrase) {
      this.walletError = "Enter the recovery phrase first.";
      this.render();
      return;
    }
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      const mode = getWalletState().mode;
      if (mode === "tor") await this.ensureWalletTor();
      await restoreWallet(
        phrase,
        this.walletNetwork,
        DEFAULT_ETHEREUM_NETWORK,
        mode,
      );
      this.walletRestoreInput = "";
      showToast("Wallet restored. Press Sync to find your balance.", {
        kind: "success",
      });
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "That phrase could not be restored.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletNewAddress(): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      await newReceiveAddress(this.walletChain);
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "A new address could not be made.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletSyncNow(): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      const state = await syncWallet(this.walletChain);
      const view = chainState(state, this.walletChain);
      showToast(
        view.confirmed !== "0"
          ? this.walletChain === "bitcoin"
            ? `Balance: ${formatBtc(view.confirmed)} BTC`
            : `Balance: ${formatEth(view.confirmed)} ETH`
          : this.walletChain === "bitcoin"
            ? "Synced. No confirmed balance yet."
            : "Refreshed. No balance yet.",
        { kind: "success" },
      );
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "The wallet could not reach the chain.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletSendNow(): Promise<void> {
    const to = this.walletSendTo.trim();
    if (!to) {
      this.walletError = "Enter a destination address.";
      this.render();
      return;
    }
    const state = getWalletState();
    const token =
      this.walletChain === "ethereum"
        ? (state.tokens.find((t) => t.address === this.walletSendToken) ?? null)
        : null;
    const decimals = token ? token.decimals : this.walletChain === "bitcoin" ? 0 : 18;
    const unit = token ? token.symbol : this.walletChain === "bitcoin" ? "sat" : "ETH";
    // The amount is converted to base units here, exactly: a value the unit
    // cannot represent is refused rather than rounded into a different amount.
    const amount = parseUnits(this.walletSendAmount, decimals);
    if (amount === null) {
      this.walletError =
        this.walletSendAmount.trim() === ""
          ? `Enter an amount in ${unit}.`
          : `That amount is not a number of ${unit} this wallet can send exactly.`;
      this.render();
      return;
    }
    if (amount === "0") {
      this.walletError = "Enter an amount greater than zero.";
      this.render();
      return;
    }
    const feeRate = Number.parseInt(this.walletSendFee, 10);
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      this.walletLastTx = await sendPayment({
        chain: this.walletChain,
        to,
        amount,
        feeRate:
          this.walletChain === "bitcoin" && Number.isFinite(feeRate) && feeRate > 0
            ? feeRate
            : null,
        token: token?.address ?? null,
      });
      this.walletSendTo = "";
      this.walletSendAmount = "";
      showToast("Sent.", { kind: "success" });
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "The payment could not be sent.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletAddToken(): Promise<void> {
    const address = this.walletTokenInput.trim();
    if (!address) return;
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      await addToken(address);
      this.walletTokenInput = "";
      showToast("Token added.", { kind: "success" });
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "That token could not be added.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletRemoveToken(address: string): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      await removeToken(address);
      // A token that is no longer tracked must not stay selected for a send.
      if (this.walletSendToken === address) this.walletSendToken = null;
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "That token could not be removed.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  /**
   * Make sure the shell's Tor is up and tell the daemon where it listens.
   *
   * The daemon cannot start Tor — the shell owns the process — so a wallet that
   * wants Tor asks for it here and then reports the port. Refuses rather than
   * falling back: a Tor wallet that quietly went direct is the one outcome this
   * must never produce, so a Tor that will not come up is an error, not a
   * downgrade.
   */
  private async ensureWalletTor(): Promise<void> {
    const st = await torStart();
    if (st.phase !== "ready" || !st.socks_port) {
      throw new AppError(
        st.message ?? "Tor could not connect, so the wallet cannot route through it.",
      );
    }
    await setWalletProxy(`socks5h://127.0.0.1:${st.socks_port}`);
  }

  private async walletChangeMode(mode: WalletNetworkMode): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      // Tor first: the daemon refuses to build a Tor wallet without a route,
      // so switching the mode before the proxy is known would fail the switch.
      if (mode === "tor") await this.ensureWalletTor();
      await setWalletMode(mode);
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "The network could not be changed.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  private async walletWipeNow(): Promise<void> {
    this.walletBusy = true;
    this.walletError = null;
    this.render();
    try {
      await wipeWallet();
      this.walletLastTx = null;
      this.walletWipeConfirm = "";
      showToast("Wallet removed from this vault.", { kind: "success" });
    } catch (e) {
      this.walletError =
        e instanceof AppError ? e.message : "The wallet could not be removed.";
    } finally {
      this.walletBusy = false;
      this.render();
    }
  }

  /** The IRC rail. The pane builds and patches its own subtree; the app mounts
   *  it once and otherwise leaves it alone. */
  private renderIrc(): HTMLElement {
    const host = document.createElement("div");
    host.className = "main-pane irc-pane";
    this.ircPane = new IrcPane(host);
    this.ircPane.mount();
    return host;
  }

  /** Patch the IRC rail badge in place while the rail is already up. */
  private updateIrcRailBadge(total: number, mentions: number): void {
    const btn = this.root.querySelector<HTMLElement>('.rail-btn[data-rail="irc"]');
    if (!btn) return;
    btn.querySelector(".rail-count")?.remove();
    btn.querySelector(".rail-badge")?.remove();
    if (total > 0) {
      const pill = document.createElement("span");
      pill.className = "rail-count";
      pill.textContent = total > 99 ? "99+" : String(total);
      btn.appendChild(pill);
    }
    if (mentions > 0) {
      const dot = document.createElement("span");
      dot.className = "rail-badge";
      dot.setAttribute("aria-label", "You were mentioned");
      btn.appendChild(dot);
    }
  }

  private renderTorrents(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane torrents-pane";
    const state = getTorrentsState();

    const head = document.createElement("div");
    head.className = "torrents-head";
    const title = document.createElement("h1");
    title.className = "text-22";
    title.textContent = "Torrents";
    const settingsBtn = document.createElement("button");
    settingsBtn.type = "button";
    settingsBtn.className = "btn-secondary text-13 pressable";
    settingsBtn.textContent = state.has_key ? "TorBox settings" : "Set up TorBox";
    settingsBtn.onclick = () => {
      this.rail = "settings";
      this.settingsSection = "torbox";
      this.render();
    };
    head.append(title, settingsBtn);
    pane.appendChild(head);

    if (!state.has_key) {
      pane.appendChild(this.renderTorrentsNoKey());
      return pane;
    }

    pane.appendChild(this.renderTorrentsAddBar());

    const active = activeTorrentCount(state);
    const tabs = document.createElement("div");
    tabs.className = "torrents-tabs";
    const tabDefs: { id: "active" | "library"; label: string }[] = [
      { id: "active", label: active > 0 ? `Active & Queue (${active})` : "Active & Queue" },
      { id: "library", label: `Library${state.library.length ? ` (${state.library.length})` : ""}` },
    ];
    for (const t of tabDefs) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = `torrents-tab pressable${this.torrentsView === t.id ? " active" : ""}`;
      b.textContent = t.label;
      b.onclick = () => {
        this.torrentsView = t.id;
        this.torrentsLibraryPath = [];
        this.render();
      };
      tabs.appendChild(b);
    }
    pane.appendChild(tabs);

    pane.appendChild(
      this.torrentsView === "active"
        ? this.renderTorrentsActive(state)
        : this.renderTorrentsLibrary(state),
    );
    return pane;
  }

  private renderTorrentsNoKey(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "torrents-empty";
    const ic = icon("torrents");
    ic.classList.add("torrents-empty-icon");
    const h = document.createElement("h2");
    h.className = "text-18";
    h.textContent = "Connect TorBox to start";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "Torrents are fetched in the cloud by TorBox, then downloaded straight into your vault. Paste your TorBox API key once to turn it on.";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn-primary pressable";
    btn.textContent = "Open Settings → TorBox";
    btn.onclick = () => {
      this.rail = "settings";
      this.settingsSection = "torbox";
      this.render();
    };
    wrap.append(ic, h, p, btn);
    return wrap;
  }

  private renderTorrentsAddBar(): HTMLElement {
    const bar = document.createElement("div");
    bar.className = `torrents-add${this.torrentsDragOver ? " drag-over" : ""}`;

    const input = document.createElement("input");
    input.className = "input-field torrents-add-input";
    input.placeholder = "Paste a magnet link or .torrent URL";
    input.value = this.torrentsAddInput;
    input.spellcheck = false;
    input.oninput = () => {
      this.torrentsAddInput = input.value;
    };
    // Auto-detect on paste: a magnet or .torrent URL is added without a
    // second click. The timeout lets the paste land in the field first.
    input.onpaste = () => {
      window.setTimeout(() => {
        const text = input.value.trim();
        if (looksLikeTorrentSource(text)) {
          this.torrentsAddInput = text;
          void this.torrentsSubmit();
        }
      }, 0);
    };
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void this.torrentsSubmit();
      }
    };

    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-primary pressable";
    add.textContent = this.torrentsAddBusy ? "Adding…" : "Add";
    add.disabled = this.torrentsAddBusy;
    add.onclick = () => void this.torrentsSubmit();

    const fileBtn = document.createElement("button");
    fileBtn.type = "button";
    fileBtn.className = "btn-secondary text-13 pressable";
    fileBtn.textContent = "Choose .torrent…";
    const fileInput = document.createElement("input");
    fileInput.type = "file";
    fileInput.accept = ".torrent,application/x-bittorrent";
    fileInput.hidden = true;
    fileInput.onchange = () => {
      const f = fileInput.files?.[0];
      if (f) void this.torrentsSubmitFile(f);
    };
    fileBtn.onclick = () => fileInput.click();

    bar.append(input, add, fileBtn, fileInput);

    const hint = document.createElement("p");
    hint.className = "muted text-13 torrents-add-hint";
    hint.textContent =
      "Or drop a .torrent file here. TorBox fetches it, then it downloads into your vault automatically.";
    bar.appendChild(hint);

    if (this.torrentsAddError) {
      const err = document.createElement("p");
      err.className = "text-13 torrents-add-error";
      err.textContent = this.torrentsAddError;
      bar.appendChild(err);
    }

    bar.ondragover = (e) => {
      e.preventDefault();
      if (!this.torrentsDragOver) {
        this.torrentsDragOver = true;
        bar.classList.add("drag-over");
      }
    };
    bar.ondragleave = () => {
      this.torrentsDragOver = false;
      bar.classList.remove("drag-over");
    };
    bar.ondrop = (e) => {
      e.preventDefault();
      this.torrentsDragOver = false;
      bar.classList.remove("drag-over");
      const f = e.dataTransfer?.files?.[0];
      if (f) void this.torrentsSubmitFile(f);
    };
    return bar;
  }

  private async torrentsSubmit(): Promise<void> {
    const source = this.torrentsAddInput.trim();
    this.torrentsAddError = null;
    if (!source) {
      this.torrentsAddError = "Paste a magnet link or .torrent URL first.";
      this.render();
      return;
    }
    if (!looksLikeTorrentSource(source)) {
      this.torrentsAddError = "That doesn't look like a magnet link or a .torrent URL.";
      this.render();
      return;
    }
    this.torrentsAddBusy = true;
    this.render();
    try {
      await addTorrent(source);
      this.torrentsAddInput = "";
      this.torrentsView = "active";
      this.announce("Torrent added");
    } catch (e) {
      this.torrentsAddError =
        e instanceof AppError ? e.message : "Could not add that torrent.";
    } finally {
      this.torrentsAddBusy = false;
      this.render();
    }
  }

  private async torrentsSubmitFile(file: File): Promise<void> {
    this.torrentsAddError = null;
    if (!/\.torrent$/i.test(file.name) && file.type !== "application/x-bittorrent") {
      this.torrentsAddError = "Drop a .torrent file (not a video or archive).";
      this.render();
      return;
    }
    this.torrentsAddBusy = true;
    this.render();
    try {
      const b64 = await readFileAsBase64(file);
      await addTorrentFile(file.name, b64);
      this.torrentsView = "active";
      this.announce("Torrent added");
    } catch (e) {
      this.torrentsAddError =
        e instanceof AppError ? e.message : "Could not read that torrent file.";
    } finally {
      this.torrentsAddBusy = false;
      this.render();
    }
  }

  private renderTorrentsActive(state: TorrentsStateDto): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "torrents-list";
    if (!state.items.length) {
      const p = document.createElement("p");
      p.className = "muted torrents-list-empty";
      p.textContent = "Nothing in the queue. Add a magnet above to get started.";
      wrap.appendChild(p);
      return wrap;
    }
    for (const item of state.items) wrap.appendChild(this.renderTorrentCard(item));
    return wrap;
  }

  private renderTorrentCard(item: TorrentItemDto): HTMLElement {
    const card = document.createElement("div");
    card.className = `torrent-card status-${item.status}`;

    const top = document.createElement("div");
    top.className = "torrent-card-top";
    const name = document.createElement("div");
    name.className = "torrent-card-name text-15";
    name.textContent = item.name;
    name.title = item.name;
    const badge = document.createElement("span");
    badge.className = `torrent-badge status-${item.status}`;
    badge.textContent = torrentStatusLabel(item);
    top.append(name, badge);
    card.appendChild(top);

    const frac =
      item.status === "downloading" || item.status === "completed"
        ? item.dl_total
          ? item.dl_done / item.dl_total
          : 0
        : item.progress;
    const bar = document.createElement("div");
    bar.className = "torrent-bar";
    const fill = document.createElement("div");
    fill.className = "torrent-bar-fill";
    fill.style.width = `${Math.max(0, Math.min(1, frac)) * 100}%`;
    bar.appendChild(fill);
    card.appendChild(bar);

    const bits: string[] = [];
    if (item.size) bits.push(formatBytes(item.size));
    if (item.status === "caching") {
      const speed = formatSpeed(item.speed);
      if (speed) bits.push(speed);
      if (item.seeds) bits.push(`${item.seeds} seed${item.seeds === 1 ? "" : "s"}`);
    }
    if (item.status === "downloading" && item.dl_total) {
      bits.push(`${formatBytes(item.dl_done)} / ${formatBytes(item.dl_total)}`);
      const speed = formatSpeed(item.dl_speed);
      if (speed) bits.push(speed);
    }
    bits.push(readsTimeAgo(item.added_at));
    const meta = document.createElement("div");
    meta.className = "torrent-meta muted text-13";
    meta.textContent = bits.join(" · ");
    card.appendChild(meta);

    if (item.status === "error" && item.message) {
      const err = document.createElement("p");
      err.className = "torrent-error text-13";
      err.textContent = item.message;
      card.appendChild(err);
    }

    const actions = document.createElement("div");
    actions.className = "torrent-actions";
    if (item.status === "paused") {
      actions.appendChild(
        this.torrentActionBtn("Resume", "play", () => this.torrentsControl(item.id, "resume")),
      );
    } else if (item.status !== "completed") {
      actions.appendChild(
        this.torrentActionBtn("Pause", "pause", () => this.torrentsControl(item.id, "pause")),
      );
    }
    if (item.status === "error") {
      actions.appendChild(
        this.torrentActionBtn("Retry", "refresh", () => this.torrentsControl(item.id, "retry")),
      );
    }
    actions.appendChild(
      this.torrentActionBtn("Remove", "trash", () => this.torrentsControl(item.id, "remove")),
    );
    card.appendChild(actions);
    return card;
  }

  private torrentActionBtn(
    label: string,
    ic: IconName,
    fn: () => Promise<void>,
  ): HTMLElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn-secondary text-13 pressable";
    b.appendChild(icon(ic));
    const s = document.createElement("span");
    s.textContent = label;
    b.appendChild(s);
    b.onclick = () => void fn();
    return b;
  }

  private async torrentsControl(
    id: string,
    action: "pause" | "resume" | "retry" | "remove",
  ): Promise<void> {
    try {
      if (action === "pause") await pauseTorrent(id);
      else if (action === "resume") await resumeTorrent(id);
      else if (action === "retry") await retryTorrent(id);
      else await removeTorrent(id);
    } catch (e) {
      showToast(e instanceof AppError ? e.message : "That action failed.", {
        kind: "error",
      });
    } finally {
      this.render();
    }
  }

  private renderTorrentsLibrary(state: TorrentsStateDto): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "torrents-library";

    // Breadcrumbs: Library / <torrent> / <subfolder…>
    const crumbs = document.createElement("div");
    crumbs.className = "torrents-crumbs";
    const rootBtn = document.createElement("button");
    rootBtn.type = "button";
    rootBtn.className = "crumb pressable";
    rootBtn.textContent = "Library";
    rootBtn.onclick = () => {
      this.torrentsLibraryPath = [];
      this.render();
    };
    crumbs.appendChild(rootBtn);
    for (let i = 0; i < this.torrentsLibraryPath.length; i++) {
      const seg = this.torrentsLibraryPath[i];
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "/";
      crumbs.appendChild(sep);
      const b = document.createElement("button");
      b.type = "button";
      b.className = "crumb pressable";
      b.textContent =
        i === 0
          ? state.library.find((e) => e.id === seg)?.name ?? "Torrent"
          : seg;
      b.onclick = () => {
        this.torrentsLibraryPath = this.torrentsLibraryPath.slice(0, i + 1);
        this.render();
      };
      crumbs.appendChild(b);
    }
    wrap.appendChild(crumbs);

    // Toolbar: search, sort, view toggle.
    const tools = document.createElement("div");
    tools.className = "torrents-tools";
    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const si = document.createElement("input");
    si.placeholder = "Search";
    si.value = this.torrentsLibrarySearch;
    si.oninput = () => {
      this.torrentsLibrarySearch = si.value;
      this.render();
    };
    search.appendChild(si);
    const sortSel = document.createElement("select");
    sortSel.className = "input-field torrents-sort";
    for (const [v, t] of [
      ["date", "Newest"],
      ["name", "Name"],
      ["size", "Size"],
    ] as const) {
      const o = document.createElement("option");
      o.value = v;
      o.textContent = t;
      sortSel.appendChild(o);
    }
    sortSel.value = this.torrentsLibrarySort;
    sortSel.onchange = () => {
      this.torrentsLibrarySort = sortSel.value as "date" | "name" | "size";
      this.render();
    };
    const viewToggle = document.createElement("div");
    viewToggle.className = "segmented torrents-view";
    for (const v of ["list", "grid"] as const) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = this.torrentsLibraryView === v ? "active" : "";
      b.appendChild(icon(v === "list" ? "listView" : "grid"));
      b.setAttribute("aria-label", v === "list" ? "List view" : "Grid view");
      b.onclick = () => {
        this.torrentsLibraryView = v;
        this.render();
      };
      viewToggle.appendChild(b);
    }
    tools.append(search, sortSel, viewToggle);
    wrap.appendChild(tools);

    const q = this.torrentsLibrarySearch.trim().toLowerCase();

    if (this.torrentsLibraryPath.length === 0) {
      // Top level: one folder per downloaded torrent.
      let entries = [...state.library];
      if (q) entries = entries.filter((e) => e.name.toLowerCase().includes(q));
      entries.sort((a, b) => {
        if (this.torrentsLibrarySort === "name") return a.name.localeCompare(b.name);
        if (this.torrentsLibrarySort === "size") return b.size - a.size;
        return b.added_at - a.added_at;
      });
      if (!entries.length) {
        const p = document.createElement("p");
        p.className = "muted torrents-list-empty";
        p.textContent = state.library.length
          ? "No torrents match that search."
          : "Nothing in the library yet. Finished torrents land here.";
        wrap.appendChild(p);
        return wrap;
      }
      const container = document.createElement("div");
      container.className = `torrents-items ${this.torrentsLibraryView}`;
      for (const e of entries) container.appendChild(this.renderLibraryTorrent(e));
      wrap.appendChild(container);
      return wrap;
    }

    // Inside a torrent: immediate children of the current subfolder.
    const entry = state.library.find((e) => e.id === this.torrentsLibraryPath[0]);
    if (!entry) {
      const p = document.createElement("p");
      p.className = "muted torrents-list-empty";
      p.textContent = "That torrent is no longer in the library.";
      wrap.appendChild(p);
      return wrap;
    }
    const sub = this.torrentsLibraryPath.slice(1);
    const nodes = new Map<string, { name: string; folder: boolean; size: number; file?: TorrentFileDto }>();
    for (const f of entry.files) {
      const parts = f.path.split("/").filter(Boolean);
      if (parts.slice(0, sub.length).join("/") !== sub.join("/")) continue;
      const rel = parts.slice(sub.length);
      if (!rel.length) continue;
      const head = rel[0];
      if (rel.length === 1) {
        nodes.set(head, { name: head, folder: false, size: f.size, file: f });
      } else if (!nodes.has(head)) {
        nodes.set(head, { name: head, folder: true, size: 0 });
      }
    }
    let list = [...nodes.values()];
    if (q) list = list.filter((n) => n.name.toLowerCase().includes(q));
    list.sort((a, b) => {
      if (a.folder !== b.folder) return a.folder ? -1 : 1;
      if (this.torrentsLibrarySort === "name") return a.name.localeCompare(b.name);
      if (this.torrentsLibrarySort === "size") return b.size - a.size;
      return a.name.localeCompare(b.name);
    });

    const deleteRow = document.createElement("div");
    deleteRow.className = "torrents-library-actions";
    deleteRow.appendChild(
      this.torrentActionBtn("Delete torrent", "trash", async () => {
        this.torrentsDeleteId = entry.id;
        this.render();
      }),
    );
    wrap.appendChild(deleteRow);

    if (!list.length) {
      const p = document.createElement("p");
      p.className = "muted torrents-list-empty";
      p.textContent = "This folder is empty.";
      wrap.appendChild(p);
      return wrap;
    }
    const container = document.createElement("div");
    container.className = `torrents-items ${this.torrentsLibraryView}`;
    for (const n of list) {
      container.appendChild(
        n.folder
          ? this.renderLibraryFolder(n.name, () => {
              this.torrentsLibraryPath = [...this.torrentsLibraryPath, n.name];
              this.render();
            })
          : this.renderLibraryFile(n.file!),
      );
    }
    wrap.appendChild(container);
    return wrap;
  }

  private renderLibraryTorrent(e: TorrentLibraryEntryDto): HTMLElement {
    const open = () => {
      this.torrentsLibraryPath = [e.id];
      this.torrentsLibrarySearch = "";
      this.render();
    };
    const el = this.libraryItemShell("folder", e.name, formatBytes(e.size), open);
    const meta = document.createElement("span");
    meta.className = "library-item-sub muted text-12";
    meta.textContent = `${e.files.length} file${e.files.length === 1 ? "" : "s"} · ${readsTimeAgo(e.added_at)}`;
    el.appendChild(meta);
    const actions = document.createElement("div");
    actions.className = "library-item-actions";
    const del = document.createElement("button");
    del.type = "button";
    del.className = "library-item-action pressable";
    del.setAttribute("aria-label", `Delete ${e.name}`);
    del.appendChild(icon("trash"));
    del.onclick = () => {
      this.torrentsDeleteId = e.id;
      this.render();
    };
    actions.appendChild(del);
    el.appendChild(actions);
    return el;
  }

  private renderLibraryFolder(name: string, open: () => void): HTMLElement {
    return this.libraryItemShell("folder", name, "", open);
  }

  private renderLibraryFile(f: TorrentFileDto): HTMLElement {
    const busy = f.blob_id != null && f.blob_id === this.torrentsBusyBlob;
    // No onOpen: the file row is not a control. Opening is one of the two
    // buttons below, so a click on the name does nothing surprising.
    const el = this.libraryItemShell(
      busy ? spinner() : torrentFileIcon(f),
      f.name,
      busy ? "Decrypting…" : formatBytes(f.size),
    );
    if (busy) el.setAttribute("aria-busy", "true");

    const actions = document.createElement("div");
    actions.className = "library-item-actions";

    // In-place open: decrypt inside the vault and hand the plaintext to the
    // OS. Green and shield-marked because it is the safe default — the file
    // never leaves the vault.
    const open = document.createElement("button");
    open.type = "button";
    open.className = "btn-library-open pressable";
    open.disabled = busy;
    open.title = `Decrypt and open ${f.name} inside the vault`;
    // Visible label first, so the accessible name still contains "Open inside
    // vault" (WCAG 2.5.3) while the file name disambiguates the row.
    open.setAttribute("aria-label", `Open inside vault: ${f.name}`);
    const openLabel = document.createElement("span");
    openLabel.textContent = "Open inside vault";
    open.append(icon("vault"), openLabel);
    open.onclick = () => void this.torrentsOpenFile(f);

    // Export: write a plaintext copy out to the filesystem. Not a download
    // arrow — nothing is fetched from a server, and that glyph already means
    // "download a torrent" elsewhere in this app.
    const exportCopy = document.createElement("button");
    exportCopy.type = "button";
    exportCopy.className = "btn-library-export pressable";
    exportCopy.disabled = busy;
    exportCopy.title = `Export ${f.name} to a folder…`;
    exportCopy.setAttribute("aria-label", `Export ${f.name} to a folder`);
    const exportLabel = document.createElement("span");
    exportLabel.textContent = "Export";
    exportCopy.append(icon("upload"), exportLabel);
    exportCopy.onclick = () => void this.torrentsSaveFile(f);

    actions.append(open, exportCopy);
    el.appendChild(actions);
    return el;
  }

  /**
   * Shared row/tile shell for a Library entry: icon, name, size.
   *
   * Deliberately a plain container, not a <button>. Callers add their own
   * controls, and a <button> inside a <button> is invalid HTML — engines
   * disagree about which one owns the click, so the inner control silently
   * stops working. Rows that open something pass `onOpen` and get a button
   * around the name area only; file rows pass nothing and stay inert, so the
   * only controls on them are the explicit action buttons.
   */
  private libraryItemShell(
    ic: IconName | HTMLElement,
    name: string,
    sizeText: string,
    onOpen?: () => void,
  ): HTMLElement {
    const el = document.createElement("div");
    el.className = "library-item";
    let main: HTMLElement;
    if (onOpen) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.onclick = onOpen;
      main = btn;
      el.classList.add("clickable");
    } else {
      main = document.createElement("div");
    }
    main.className = "library-item-main";
    const icEl = typeof ic === "string" ? icon(ic) : ic;
    icEl.classList.add("library-item-icon");
    const label = document.createElement("span");
    label.className = "library-item-name text-15";
    label.textContent = name;
    label.title = name;
    main.append(icEl, label);
    if (sizeText) {
      const size = document.createElement("span");
      size.className = "library-item-size muted text-12";
      size.textContent = sizeText;
      main.appendChild(size);
    }
    el.appendChild(main);
    return el;
  }

  private async torrentsOpenFile(f: TorrentFileDto): Promise<void> {
    if (!f.blob_id) {
      showToast("That file isn't stored in the vault yet.", { kind: "error" });
      return;
    }
    if (this.torrentsBusyBlob) return;
    const blobId = f.blob_id;
    this.beginTorrentsBusy(blobId, f.name, f.size);
    try {
      await openVaultBlob(f.name, blobId);
    } catch (e) {
      showToast(e instanceof Error ? e.message : "Could not open that file.", {
        kind: "error",
      });
    } finally {
      this.endTorrentsBusy();
    }
  }

  private async torrentsSaveFile(f: TorrentFileDto): Promise<void> {
    if (!f.blob_id) {
      showToast("That file isn't stored in the vault yet.", { kind: "error" });
      return;
    }
    if (this.torrentsBusyBlob) return;
    const blobId = f.blob_id;
    this.beginTorrentsBusy(blobId, f.name, f.size);
    try {
      await saveVaultBlob(f.name, blobId);
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Could not save that file.";
      if (!msg.toLowerCase().includes("cancel")) showToast(msg, { kind: "error" });
    } finally {
      this.endTorrentsBusy();
    }
  }

  /**
   * Decrypting happens on the daemon and is not observable from here, so mark
   * the row busy and hold a toast open until the action settles. A large
   * torrent takes a while, and silence reads as a broken click.
   */
  private beginTorrentsBusy(blobId: string, name: string, size: number): void {
    this.torrentsBusyBlob = blobId;
    this.torrentsBusyToast = showToast(
      `Decrypting ${name} (${formatBytes(size)})…`,
      { kind: "info", timeoutMs: 0 },
    );
    this.render();
  }

  private endTorrentsBusy(): void {
    this.torrentsBusyBlob = null;
    this.torrentsBusyToast?.();
    this.torrentsBusyToast = null;
    this.render();
  }

  private renderTorrentsDeleteModal(): HTMLElement {
    const id = this.torrentsDeleteId!;
    const entry = getTorrentsState().library.find((e) => e.id === id);
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Delete from library");

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "Delete from library?";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-3)";
    p.textContent = entry
      ? `“${entry.name}” and its ${entry.files.length} file${entry.files.length === 1 ? "" : "s"} will be removed from your vault. This cannot be undone.`
      : "This torrent will be removed from your vault. This cannot be undone.";

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.torrentsDeleteId = null;
      this.render();
    };
    const del = document.createElement("button");
    del.type = "button";
    del.className = "btn-primary pressable";
    del.style.background = "var(--danger)";
    del.textContent = "Delete";
    del.onclick = async () => {
      del.disabled = true;
      try {
        await deleteLibraryEntry(id);
        this.announce("Deleted from library");
      } catch (e) {
        showToast(e instanceof AppError ? e.message : "Could not delete that.", {
          kind: "error",
        });
      } finally {
        this.torrentsDeleteId = null;
        this.torrentsLibraryPath = [];
        this.render();
      }
    };
    row.append(cancel, del);
    modal.append(h, p, row);
    back.appendChild(modal);
    back.onclick = (ev) => {
      if (ev.target === back) {
        this.torrentsDeleteId = null;
        this.render();
      }
    };
    return back;
  }

  private renderTorboxSettings(): HTMLElement {
    const state = getTorrentsState();
    const body = document.createElement("div");
    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = "TorBox";
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.textContent =
      "TorBox does the torrenting in the cloud; Hush downloads the finished files into your vault. Your API key is stored encrypted inside the vault and never logged or shown in a URL.";
    body.append(h, p);

    // Current status line.
    const status = document.createElement("p");
    status.className = "text-13";
    status.style.marginTop = "var(--space-3)";
    if (state.has_key) {
      const plan = state.plan_label ? ` · ${state.plan_label} plan` : "";
      const email = state.email ? ` (${state.email})` : "";
      status.textContent = `Connected${plan}${email}`;
      status.style.color = "var(--success-text)";
    } else {
      status.textContent = "No API key set.";
      status.className = "muted text-13";
    }
    body.appendChild(status);

    // Key field with show/hide.
    const label = document.createElement("p");
    label.className = "muted";
    label.style.marginTop = "var(--space-4)";
    label.textContent = "API key";
    const field = document.createElement("div");
    field.className = "torbox-key-field";
    const input = document.createElement("input");
    input.className = "input-field";
    input.type = this.torboxKeyVisible ? "text" : "password";
    input.placeholder = state.has_key ? "•••••• (saved) — enter to replace" : "Paste your TorBox API key";
    input.value = this.torboxKeyDraft;
    input.autocomplete = "off";
    input.spellcheck = false;
    input.oninput = () => {
      this.torboxKeyDraft = input.value;
    };
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "btn-icon-round pressable";
    toggle.setAttribute("aria-label", this.torboxKeyVisible ? "Hide key" : "Show key");
    toggle.appendChild(icon(this.torboxKeyVisible ? "hide" : "eye"));
    toggle.onclick = () => {
      this.torboxKeyVisible = !this.torboxKeyVisible;
      this.render();
    };
    field.append(input, toggle);
    body.append(label, field);

    // Test + Save.
    const actions = document.createElement("div");
    actions.className = "torbox-actions";
    const test = document.createElement("button");
    test.type = "button";
    test.className = "btn-secondary pressable";
    test.textContent = this.torboxBusy ? "Working…" : "Test connection";
    test.disabled = this.torboxBusy;
    test.onclick = async () => {
      this.torboxBusy = true;
      this.torboxTestOk = null;
      this.torboxTestError = null;
      this.render();
      try {
        const info = await testTorboxKey(this.torboxKeyDraft);
        this.torboxTestOk = `Connected — ${info.plan_label} plan${info.email ? ` (${info.email})` : ""}.`;
      } catch (e) {
        this.torboxTestError = e instanceof AppError ? e.message : "TorBox rejected that key.";
      } finally {
        this.torboxBusy = false;
        this.render();
      }
    };
    const save = document.createElement("button");
    save.type = "button";
    save.className = "btn-primary pressable";
    save.textContent = this.torboxBusy ? "Working…" : "Save";
    // An empty field means "no change" when a key is already saved; clearing is
    // an explicit action, so a stray Save cannot silently disconnect TorBox.
    save.disabled = this.torboxBusy || !this.torboxKeyDraft.trim();
    save.onclick = async () => {
      this.torboxBusy = true;
      this.torboxTestOk = null;
      this.torboxTestError = null;
      this.render();
      try {
        await saveTorboxKey(this.torboxKeyDraft);
        this.torboxKeyDraft = "";
        showToast("TorBox key saved", { kind: "success" });
      } catch (e) {
        this.torboxTestError = e instanceof AppError ? e.message : "Could not save that key.";
      } finally {
        this.torboxBusy = false;
        this.render();
      }
    };
    actions.append(test, save);
    if (state.has_key) {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "btn-secondary pressable";
      remove.textContent = "Remove key";
      remove.disabled = this.torboxBusy;
      remove.onclick = async () => {
        this.torboxBusy = true;
        this.render();
        try {
          await saveTorboxKey("");
          this.torboxKeyDraft = "";
          showToast("TorBox key removed", { kind: "info" });
        } catch (e) {
          this.torboxTestError =
            e instanceof AppError ? e.message : "Could not remove that key.";
        } finally {
          this.torboxBusy = false;
          this.render();
        }
      };
      actions.appendChild(remove);
    }
    body.appendChild(actions);

    const feedback = document.createElement("p");
    feedback.className = "text-13";
    feedback.style.marginTop = "var(--space-3)";
    if (this.torboxTestError) {
      feedback.style.color = "var(--danger)";
      feedback.textContent = this.torboxTestError;
    } else if (this.torboxTestOk) {
      feedback.textContent = this.torboxTestOk;
    } else {
      feedback.hidden = true;
    }
    body.appendChild(feedback);

    // Concurrency.
    const concLabel = document.createElement("p");
    concLabel.className = "muted";
    concLabel.style.marginTop = "var(--space-5)";
    concLabel.textContent = "Simultaneous vault downloads";
    const conc = document.createElement("select");
    conc.className = "input-field";
    conc.style.marginTop = "var(--space-2)";
    for (const n of [1, 2, 3, 4]) {
      const o = document.createElement("option");
      o.value = String(n);
      o.textContent = n === 1 ? "1 file" : `${n} files`;
      conc.appendChild(o);
    }
    conc.value = String(state.max_concurrent);
    conc.onchange = async () => {
      try {
        await setTorrentConcurrency(Number(conc.value));
      } catch (e) {
        showToast(e instanceof AppError ? e.message : "Could not change that.", {
          kind: "error",
        });
        this.render();
      }
    };
    const concHint = document.createElement("p");
    concHint.className = "muted text-13";
    concHint.style.marginTop = "var(--space-2)";
    concHint.textContent = "How many files download from TorBox at once. 2–3 suits most connections.";
    body.append(concLabel, conc, concHint);

    // A refresh of the account's plan, and a jump to the pane.
    const footer = document.createElement("div");
    footer.className = "torbox-actions";
    footer.style.marginTop = "var(--space-5)";
    const refresh = document.createElement("button");
    refresh.type = "button";
    refresh.className = "btn-secondary pressable";
    refresh.textContent = "Refresh account";
    refresh.onclick = async () => {
      try {
        await refreshTorboxAccount();
      } catch (e) {
        showToast(e instanceof AppError ? e.message : "Could not refresh.", { kind: "error" });
      } finally {
        this.render();
      }
    };
    const open = document.createElement("button");
    open.type = "button";
    open.className = "btn-secondary pressable";
    open.textContent = "Open Torrents";
    open.onclick = () => {
      this.rail = "torrents";
      this.render();
    };
    footer.append(refresh, open);
    body.appendChild(footer);
    return body;
  }

  private renderVault(): HTMLElement {    const pane = document.createElement("div");
    pane.className = "main-pane";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.style.padding = "var(--space-5)";
    h.textContent = "Vault";
    const grid = document.createElement("div");
    grid.className = "bento";
    const s = this.status!;
    const storage = document.createElement("div");
    storage.className = "tile";
    storage.innerHTML = `<div class="text-15">Storage</div><div class="text-28">${formatBytes(s.vault_bytes)}</div><button class="btn-secondary pressable" style="margin-top:12px">Manage</button>`;
    const lock = document.createElement("div");
    lock.className = "tile";
    const mins = getAutoLockMinutes();
    const idleLabel =
      mins <= 0 ? "Idle lock off" : `${mins} minute${mins === 1 ? "" : "s"}`;
    const remain = Math.max(0, getAutoLockMs() - (Date.now() - this.unlockedAt));
    const countdown =
      mins <= 0
        ? "Only manual lock or suspend"
        : `${Math.ceil(remain / 60000)}m until lock`;
    lock.innerHTML = `<div class="text-15">Auto-lock</div><div class="muted text-13">${idleLabel}</div><div class="text-13">${countdown}</div><button class="btn-secondary pressable lock-now" style="margin-top:12px">Lock now</button>`;
    lock.querySelector(".lock-now")!.addEventListener("click", () => void this.lock());
    const backup = document.createElement("div");
    backup.className = "tile";
    const lastBackup = s.last_backup_unix ?? 0;
    if (!lastBackup) backup.style.background = "var(--warning-soft)";
    const backupHint =
      lastBackup > 0
        ? `Last backup ${formatRelative(lastBackup * 1000)}`
        : "Back up your vault so you never lose your chats";
    backup.innerHTML = `<div class="text-15">Backup</div><div class="muted text-13">${backupHint}</div><button type="button" class="btn-secondary pressable backup-btn" style="margin-top:12px">Back up now</button>`;
    backup.querySelector(".backup-btn")!.addEventListener("click", async () => {
      try {
        const r = await api.backupExport();
        if (r) await downloadBase64File(r.filename, r.data_base64);
        await this.refreshStatus();
        this.announce("Backup downloaded");
        this.render();
      } catch (e) {
        this.announce(e instanceof AppError ? e.message : "Backup failed");
      }
    });
    const filesTile = document.createElement("div");
    filesTile.className = "tile";
    const fileCount =
      allFileMessages().length +
      this.contacts.reduce((n, c) => n + this.mailAttachmentCount(c.nickname), 0);
    filesTile.innerHTML = `<div class="text-15">Exchanged files</div><div class="muted text-13">${fileCount} in your sealed vault (encrypted blobs)</div><button type="button" class="btn-secondary pressable files-open-btn" style="margin-top:12px">Open Files</button>`;
    filesTile.querySelector(".files-open-btn")!.addEventListener("click", () => {
      this.filesOpenFiles();
    });
    const torrentsState = getTorrentsState();
    const torrentsTile = document.createElement("div");
    torrentsTile.className = "tile";
    const torrentBytes = torrentsState.library.reduce((sum, e) => sum + e.size, 0);
    const torrentsHint = torrentsState.has_key
      ? `${torrentsState.library.length} torrent${torrentsState.library.length === 1 ? "" : "s"} · ${formatBytes(torrentBytes)} from TorBox`
      : "Connect TorBox to download torrents into your vault";
    torrentsTile.innerHTML = `<div class="text-15">Torrents</div><div class="muted text-13">${torrentsHint}</div><button type="button" class="btn-secondary pressable torrents-tile-btn" style="margin-top:12px">Open Torrents</button>`;
    torrentsTile.querySelector(".torrents-tile-btn")!.addEventListener("click", () => {
      this.rail = "torrents";
      this.render();
    });
    const notesCount = peekNotesState().notes.length;
    const notesTile = document.createElement("div");
    notesTile.className = "tile";
    const notesHint =
      notesCount === 0
        ? "Private sticky notes, encrypted in your vault"
        : `${notesCount} note${notesCount === 1 ? "" : "s"} on your private board`;
    notesTile.innerHTML = `<div class="text-15">Notes</div><div class="muted text-13">${notesHint}</div><button type="button" class="btn-secondary pressable notes-tile-btn" style="margin-top:12px">Open Notes</button>`;
    notesTile.querySelector(".notes-tile-btn")!.addEventListener("click", () => {
      this.notesOpenNotes();
    });
    const walletState = getWalletState();
    const walletTile = document.createElement("div");
    walletTile.className = "tile";
    const btc = chainState(walletState, "bitcoin");
    const walletHint = isWalletLoaded()
      ? walletState.exists
        ? `${formatBtc(btc.confirmed)} BTC · keys sealed in this vault`
        : "No wallet yet — create one, keys stay in this vault"
      : "Self-custody Bitcoin and Ethereum, sealed in this vault";
    walletTile.innerHTML = `<div class="text-15">Wallet</div><div class="muted text-13">${walletHint}</div><button type="button" class="btn-secondary pressable wallet-tile-btn" style="margin-top:12px">Open Wallet</button>`;
    walletTile.querySelector(".wallet-tile-btn")!.addEventListener("click", () => {
      this.walletOpenWallet();
    });
    grid.append(storage, lock, backup, filesTile, torrentsTile, notesTile, walletTile);
    pane.append(h, grid);
    return pane;
  }

  // --- Notes (private sticky-paper board) ---------------------------------
  //
  // Reached from the Vault bento tile. The board renders in the Vault pane
  // (full-bleed, with a Back button) rather than a rail of its own; the list
  // and a plain editor are all 16.1 ships. Canvas, regions and checklists
  // come later.

  /** Load the board once, then repaint if the vault pane is still up. */
  private notesEnsureLoaded(): void {
    if (this.notesState || this.notesLoading) return;
    this.notesLoad();
  }

  private notesLoad(): void {
    this.notesLoading = true;
    this.notesError = null;
    void getNotesState()
      .then((state) => {
        this.notesState = state;
      })
      .catch((e) => {
        this.notesError = e instanceof Error ? e.message : String(e);
      })
      .finally(() => {
        this.notesLoading = false;
        if (this.rail === "vault") this.render();
      });
  }

  /** Open the board from the Vault tile: full-bleed, list first. */
  private notesOpenNotes(): void {
    this.notesOpen = true;
    this.walletOpen = false;
    this.notesEditorOpen = false;
    this.notesEditingId = null;
    this.notesError = null;
    this.render();
    this.notesEnsureLoaded();
  }

  private notesBackToVault(): void {
    this.notesOpen = false;
    this.notesEditorOpen = false;
    this.notesEditingId = null;
    this.notesDeleteId = null;
    this.notesViewingId = null;
    this.notesSearch = "";
    this.render();
  }

  private notesBackToList(): void {
    this.notesEditorOpen = false;
    this.notesEditingId = null;
    this.notesDraftTitle = "";
    this.notesDraftBody = "";
    this.notesDeleteId = null;
    this.notesViewingId = null;
    this.render();
  }

  /** Open a blank editor; the note is created on first save, not here. */
  private notesNewNote(): void {
    this.notesEditorOpen = true;
    this.notesEditingId = null;
    this.notesDraftTitle = "";
    this.notesDraftBody = "";
    this.notesDeleteId = null;
    this.notesViewingId = null;
    this.notesSearch = "";
    this.render();
  }

  /**
   * Clicking a note opens the read-only popup, not the editor. The editor is
   * one more click away, behind the popup's Edit button.
   */
  private notesViewNote(id: string): void {
    this.notesViewingId = id;
    this.notesDeleteId = null;
    this.render();
  }

  private notesCloseView(): void {
    this.notesViewingId = null;
    this.notesDeleteId = null;
    this.render();
  }

  private notesOpenNote(id: string): void {
    const note = (this.notesState?.notes ?? []).find((n) => n.id === id);
    if (!note) return;
    this.notesEditorOpen = true;
    this.notesEditingId = id;
    this.notesDraftTitle = note.title;
    this.notesDraftBody = note.body;
    this.notesDeleteId = null;
    this.notesViewingId = null;
    this.render();
  }

  private notesSaveDraft(): void {
    if (this.notesSaving) return;
    this.notesSaving = true;
    this.notesError = null;
    this.render();
    void saveNote({
      id: this.notesEditingId ?? undefined,
      title: this.notesDraftTitle,
      body: this.notesDraftBody,
    })
      .then(({ state }) => {
        this.notesState = state;
        this.notesSaving = false;
        this.notesEditorOpen = false;
        this.notesEditingId = null;
        this.notesDraftTitle = "";
        this.notesDraftBody = "";
        this.render();
        showToast("Note saved", { kind: "success" });
      })
      .catch((e) => {
        this.notesSaving = false;
        this.notesError = e instanceof Error ? e.message : String(e);
        this.render();
      });
  }

  private notesRequestDelete(id: string): void {
    this.notesDeleteId = id;
    this.render();
  }

  private notesCancelDelete(): void {
    this.notesDeleteId = null;
    this.render();
  }

  private notesConfirmDelete(): void {
    const id = this.notesDeleteId;
    if (!id) return;
    this.notesDeleteId = null;
    void deleteNoteRecord(id)
      .then((state) => {
        this.notesState = state;
        if (this.notesEditingId === id) {
          this.notesBackToList();
        } else if (this.notesViewingId === id) {
          this.notesCloseView();
        } else {
          this.render();
        }
        showToast("Note deleted");
      })
      .catch((e) => {
        this.notesError = e instanceof Error ? e.message : String(e);
        this.render();
      });
  }

  private renderNotes(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane notes-pane";

    const header = document.createElement("div");
    header.className = "notes-header";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary pressable notes-back";
    back.textContent = "← Vault";
    back.addEventListener("click", () => this.notesBackToVault());
    const title = document.createElement("h1");
    title.className = "text-22";
    title.textContent = "Notes";
    header.append(back, title);
    if (!this.notesEditorOpen) {
      const add = document.createElement("button");
      add.type = "button";
      add.className = "btn-primary pressable notes-new";
      add.textContent = "New note";
      add.addEventListener("click", () => this.notesNewNote());
      header.appendChild(add);
    }
    pane.appendChild(header);

    if (this.notesError) {
      const err = document.createElement("div");
      err.className = "notes-error text-13";
      err.textContent = this.notesError;
      pane.appendChild(err);
    }

    if (this.notesLoading && !this.notesState) {
      const loading = document.createElement("div");
      loading.className = "muted text-13 notes-loading";
      loading.textContent = "Loading notes…";
      pane.appendChild(loading);
      return pane;
    }

    if (this.notesEditorOpen) {
      pane.appendChild(this.notesEditor());
      return pane;
    }

    // The list is rebuilt in place on each keystroke rather than through a full
    // app render, so the search field keeps focus while filtering.
    const list = document.createElement("div");
    list.className = "notes-list";
    this.notesRenderRows(list);

    const state = this.notesState ?? peekNotesState();
    if (state.notes.length) {
      const bar = document.createElement("div");
      bar.className = "notes-search-bar";
      const input = document.createElement("input");
      input.type = "search";
      input.className = "input-field notes-search";
      input.placeholder = "Search notes…";
      input.value = this.notesSearch;
      input.setAttribute("aria-label", "Search notes");
      input.addEventListener("input", () => {
        this.notesSearch = input.value;
        this.notesRenderRows(list);
      });
      bar.appendChild(input);
      pane.append(bar, list);
    } else {
      pane.appendChild(list);
    }
    return pane;
  }

  /** Fill the list container with rows for the current query, in place. */
  private notesRenderRows(container: HTMLElement): void {
    container.innerHTML = "";
    const state = this.notesState ?? peekNotesState();
    const notes = searchNotes(state, this.notesSearch);
    if (!notes.length) {
      const empty = document.createElement("div");
      empty.className = "notes-empty";
      const msg = document.createElement("div");
      msg.className = "muted text-15";
      if (state.notes.length && this.notesSearch.trim()) {
        msg.textContent = `No notes match “${this.notesSearch.trim()}”.`;
        empty.appendChild(msg);
      } else {
        msg.textContent = "No notes yet.";
        const cta = document.createElement("button");
        cta.type = "button";
        cta.className = "btn-primary pressable";
        cta.textContent = "New note";
        cta.addEventListener("click", () => this.notesNewNote());
        empty.append(msg, cta);
      }
      container.appendChild(empty);
      return;
    }
    for (const note of notes) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = `note-row pressable paper-${note.color}`;
      const t = document.createElement("div");
      t.className = "note-row-title text-15";
      t.textContent = note.title.trim() || "Untitled";
      const p = document.createElement("div");
      p.className = "note-row-preview muted text-13";
      p.textContent = notePreview(note) || "Empty note";
      const meta = document.createElement("div");
      meta.className = "note-row-meta muted text-12";
      meta.textContent = formatRelative(note.updated_at * 1000);
      row.append(t, p, meta);
      row.addEventListener("click", () => this.notesViewNote(note.id));
      container.appendChild(row);
    }
  }

  /**
   * The read-only note popup: a centred sticky with the note's full text and
   * the actions that leave it — Edit (into the editor), Delete, Close. Rendered
   * at the app root so it sits above the whole shell, like the other modals.
   */
  private renderNoteViewModal(): HTMLElement {
    const id = this.notesViewingId!;
    const note = (this.notesState?.notes ?? []).find((n) => n.id === id);

    const back = document.createElement("div");
    back.className = "modal-backdrop";
    back.onclick = (e) => {
      if (e.target === back) this.notesCloseView();
    };

    const modal = document.createElement("div");
    // The note's paper colour drives the sticky's fill, like a real pad of
    // sticky notes where each colour is its own pad.
    modal.className = `modal note-view-modal${note ? ` paper-${note.color}` : ""}`;
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", note?.title.trim() || "Note");

    if (!note) {
      // Deleted or rotated away underneath us; offer the only useful exit.
      modal.classList.add("note-view-modal--empty");
      const gone = document.createElement("p");
      gone.className = "muted text-15";
      gone.textContent = "This note is no longer here.";
      const close = document.createElement("button");
      close.type = "button";
      close.className = "btn-secondary pressable";
      close.textContent = "Close";
      close.addEventListener("click", () => this.notesCloseView());
      modal.append(gone, close);
      back.appendChild(modal);
      return back;
    }

    const title = document.createElement("h2");
    title.className = "text-22 note-view-title";
    title.textContent = note.title.trim() || "Untitled";

    const meta = document.createElement("div");
    meta.className = "muted text-12 note-view-meta";
    meta.textContent = `Edited ${formatRelative(note.updated_at * 1000)}`;

    const body = document.createElement("div");
    body.className = "note-view-body text-15";
    body.textContent = note.body || "Empty note";

    const actions = document.createElement("div");
    actions.className = "notes-editor-actions note-view-actions";

    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "btn-primary pressable";
    edit.textContent = "Edit";
    edit.addEventListener("click", () => this.notesOpenNote(id));

    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn-secondary pressable";
    close.textContent = "Close";
    close.addEventListener("click", () => this.notesCloseView());

    actions.append(edit, close);

    if (this.notesDeleteId === id) {
      const confirm = document.createElement("div");
      confirm.className = "notes-confirm";
      const q = document.createElement("span");
      q.className = "text-13";
      q.textContent = "Delete this note?";
      const yes = document.createElement("button");
      yes.type = "button";
      yes.className = "btn-danger pressable";
      yes.textContent = "Delete";
      yes.addEventListener("click", () => this.notesConfirmDelete());
      const no = document.createElement("button");
      no.type = "button";
      no.className = "btn-secondary pressable";
      no.textContent = "Cancel";
      no.addEventListener("click", () => this.notesCancelDelete());
      confirm.append(q, yes, no);
      actions.appendChild(confirm);
    } else {
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn-secondary pressable notes-delete";
      del.textContent = "Delete";
      del.addEventListener("click", () => this.notesRequestDelete(id));
      actions.appendChild(del);
    }

    modal.append(title, meta, body, actions);
    back.appendChild(modal);
    return back;
  }

  private notesEditor(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "notes-editor";

    const titleInput = document.createElement("input");
    titleInput.type = "text";
    titleInput.className = "notes-title-input";
    titleInput.placeholder = "Title";
    titleInput.value = this.notesDraftTitle;
    titleInput.addEventListener("input", () => {
      this.notesDraftTitle = titleInput.value;
    });

    const bodyInput = document.createElement("textarea");
    bodyInput.className = "notes-body-input";
    bodyInput.placeholder = "Write a note…";
    bodyInput.value = this.notesDraftBody;
    bodyInput.addEventListener("input", () => {
      this.notesDraftBody = bodyInput.value;
    });

    const actions = document.createElement("div");
    actions.className = "notes-editor-actions";

    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary pressable";
    back.textContent = "Back to list";
    back.addEventListener("click", () => this.notesBackToList());

    const save = document.createElement("button");
    save.type = "button";
    save.className = "btn-primary pressable";
    save.textContent = this.notesSaving ? "Saving…" : "Save";
    save.disabled = this.notesSaving;
    save.addEventListener("click", () => this.notesSaveDraft());

    actions.append(back, save);

    if (this.notesEditingId) {
      if (this.notesDeleteId === this.notesEditingId) {
        const confirm = document.createElement("div");
        confirm.className = "notes-confirm";
        const q = document.createElement("span");
        q.className = "text-13";
        q.textContent = "Delete this note?";
        const yes = document.createElement("button");
        yes.type = "button";
        yes.className = "btn-danger pressable";
        yes.textContent = "Delete";
        yes.addEventListener("click", () => this.notesConfirmDelete());
        const no = document.createElement("button");
        no.type = "button";
        no.className = "btn-secondary pressable";
        no.textContent = "Cancel";
        no.addEventListener("click", () => this.notesCancelDelete());
        confirm.append(q, yes, no);
        actions.appendChild(confirm);
      } else {
        const del = document.createElement("button");
        del.type = "button";
        del.className = "btn-secondary pressable notes-delete";
        del.textContent = "Delete";
        del.addEventListener("click", () => this.notesRequestDelete(this.notesEditingId!));
        actions.appendChild(del);
      }
    }

    wrap.append(titleInput, bodyInput, actions);
    // On a fresh note, start typing in the title.
    if (!this.notesEditingId) queueMicrotask(() => titleInput.focus());
    return wrap;
  }

  /**
   * Rotate the vault passphrase.
   *
   * The daemon re-wraps the vault key rather than re-encrypting anything, so
   * this is fast regardless of vault size and the recovery code keeps working.
   * All three fields are validated here so an obvious typo never reaches the
   * daemon; the current passphrase itself can only be checked server-side.
   */
  private renderChangePassphraseForm(): HTMLElement {
    const form = document.createElement("form");
    form.style.marginTop = "var(--space-6)";

    const title = document.createElement("h3");
    title.className = "text-15";
    title.textContent = "Change passphrase";

    const note = document.createElement("p");
    note.className = "muted text-13";
    note.textContent =
      "Your messages, files and recovery code keep working — nothing is re-encrypted.";

    const fields: HTMLInputElement[] = [];
    const makeField = (placeholder: string, autocomplete: AutoFill) => {
      const input = document.createElement("input");
      input.type = "password";
      input.className = "input-field";
      input.placeholder = placeholder;
      input.autocomplete = autocomplete;
      input.style.marginTop = "var(--space-3)";
      fields.push(input);
      return input;
    };
    const current = makeField("Current passphrase", "current-password");
    const next = makeField("New passphrase", "new-password");
    const confirm = makeField("Confirm new passphrase", "new-password");

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.style.marginTop = "var(--space-2)";
    err.hidden = true;

    const submit = document.createElement("button");
    submit.type = "submit";
    submit.className = "btn-primary pressable";
    submit.style.marginTop = "var(--space-4)";
    submit.textContent = "Change passphrase";

    const showError = (message: string) => {
      err.textContent = message;
      err.hidden = false;
      form.classList.add("shake");
      setTimeout(() => form.classList.remove("shake"), 300);
    };

    form.onsubmit = async (ev) => {
      ev.preventDefault();
      err.hidden = true;
      if (fields.some((f) => !f.value)) {
        showError("Fill in all three fields.");
        return;
      }
      if (next.value !== confirm.value) {
        showError("The new passphrases don't match.");
        return;
      }
      if (next.value === current.value) {
        showError("The new passphrase must be different from the current one.");
        return;
      }
      submit.disabled = true;
      try {
        await api.changePassphrase(current.value, next.value);
        for (const f of fields) f.value = "";
        this.announce("Passphrase changed. Your recovery code still works.");
        this.render();
      } catch (e) {
        showError(
          e instanceof AppError ? e.message : "Could not change the passphrase.",
        );
      } finally {
        submit.disabled = false;
      }
    };

    form.append(title, note, current, next, confirm, err, submit);
    return form;
  }

  /** A slide switch: hidden checkbox plus a styled track, so the switch is
   * the control everywhere Tor is toggled (login card, relay settings). */
  private makeSwitch(checked: boolean): { el: HTMLElement; input: HTMLInputElement } {
    const label = document.createElement("label");
    label.className = "hush-switch";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = checked;
    const track = document.createElement("span");
    track.className = "hush-switch-track";
    label.append(input, track);
    return { el: label, input };
  }

  /** The relay section: the Tor switch and the onion address, the two pieces
   * of relay setup that outlive a login. The top-bar pill links here. */
  private renderRelaySettings(): HTMLElement {
    const wrap = document.createElement("div");
    const head = document.createElement("h2");
    head.className = "text-22";
    head.textContent = "Relay";
    const sub = document.createElement("p");
    sub.className = "muted text-13";
    sub.textContent =
      "Your relay carries encrypted mail only. With Tor on, it reaches you through an onion address and never learns your IP.";
    wrap.append(head, sub);

    if (this.torActionError) {
      const warn = document.createElement("p");
      warn.className = "text-13";
      warn.style.color = "var(--danger)";
      warn.textContent = this.torActionError;
      wrap.appendChild(warn);
    }

    if (this.torMode && !this.torApplied && !this.torOnionUrl) {
      const pending = document.createElement("p");
      pending.className = "text-13";
      pending.style.color = "var(--warning-text)";
      pending.textContent =
        "Tor is on, but no onion address is configured for this relay yet, so it cannot protect traffic. Everything else connects on its own.";
      wrap.appendChild(pending);
    }

    const switchRow = document.createElement("div");
    switchRow.style.display = "flex";
    switchRow.style.alignItems = "center";
    switchRow.style.gap = "10px";
    switchRow.style.marginTop = "var(--space-4)";
    const sw = this.makeSwitch(this.torMode);
    sw.input.onchange = async () => {
      const want = sw.input.checked;
      sw.input.disabled = true;
      try {
        await this.toggleTorRelay(want);
      } catch (e) {
        this.torActionError =
          e instanceof AppError ? e.message : "The switch could not be applied.";
        sw.input.checked = !want;
        sw.input.disabled = false;
        this.render();
      }
    };
    const swLabel = document.createElement("span");
    swLabel.className = "text-13";
    swLabel.textContent = "Use Tor for the relay (onion)";
    switchRow.append(sw.el, swLabel);
    wrap.appendChild(switchRow);

    const note = document.createElement("p");
    note.className = "faint text-12";
    note.style.marginTop = "var(--space-3)";
    note.textContent =
      "The relay's addresses come from this app's configuration — the switch decides which one is used. When Tor is on and it fails, nothing falls back to clearnet: mail waits in the outbox until Tor returns or you turn the switch off.";
    wrap.appendChild(note);
    return wrap;
  }

  private renderSettings(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane settings-layout";
    const nav = document.createElement("nav");
    nav.className = "settings-nav";
    const sections: { id: SettingsSection; label: string }[] = [
      { id: "appearance", label: "Appearance" },
      { id: "profile", label: "Profile" },
      { id: "relay", label: "Relay" },
      { id: "drops", label: "Drops" },
      { id: "torbox", label: "TorBox" },
      { id: "notifications", label: "Notifications" },
      { id: "security", label: "Security" },
      { id: "recovery", label: "Recovery" },
      { id: "backup", label: "Backup" },
      { id: "about", label: "About" },
    ];
    for (const s of sections) {
      const b = document.createElement("button");
      b.className = this.settingsSection === s.id ? "active" : "";
      b.textContent = s.label;
      b.onclick = () => {
        this.settingsSection = s.id;
        this.render();
      };
      nav.appendChild(b);
    }
    const body = document.createElement("div");
    body.className = "settings-body";
    if (this.settingsSection === "appearance") {
      body.innerHTML = `<h2 class="text-22">Appearance</h2><p class="muted">Theme</p>`;
      const seg = document.createElement("div");
      seg.className = "segmented";
      for (const mode of ["system", "light", "dark"] as ThemeMode[]) {
        const b = document.createElement("button");
        b.className = getThemeMode() === mode ? "active" : "";
        b.textContent = mode.charAt(0).toUpperCase() + mode.slice(1);
        b.onclick = () => {
          void setThemeMode(mode).then(() => this.render());
        };
        seg.appendChild(b);
      }
      body.appendChild(seg);
    } else if (this.settingsSection === "relay") {
      body.appendChild(this.renderRelaySettings());
    } else if (this.settingsSection === "torbox") {
      body.appendChild(this.renderTorboxSettings());
    } else if (this.settingsSection === "profile") {
      body.innerHTML = `<h2 class="text-22">Profile</h2><p class="muted text-13">Your mail address is a label bound to your keys, shared inside your invite. There is no public directory — contacts learn it only when you add each other.</p>`;
      const current = document.createElement("p");
      current.className = "text-15";
      current.style.marginTop = "var(--space-4)";
      current.textContent = this.mailAddress()
        ? `Your address: ${this.mailAddress()}`
        : "No address yet";
      const label = document.createElement("p");
      label.className = "muted";
      label.style.marginTop = "var(--space-4)";
      label.textContent = "Address";
      const input = document.createElement("input");
      input.className = "input-field";
      input.placeholder = "sandy";
      input.value = this.mailHandle ?? "";
      input.maxLength = 64;
      const suffix = document.createElement("span");
      suffix.className = "muted text-13 handle-suffix";
      suffix.textContent = `@${this.vaultName}`;
      const err = document.createElement("p");
      err.className = "text-13";
      err.style.color = "var(--danger)";
      err.hidden = true;
      const save = document.createElement("button");
      save.type = "button";
      save.className = "btn-primary pressable";
      save.textContent = "Save";
      const row = document.createElement("div");
      row.className = "handle-edit";
      row.append(input, suffix, save);
      save.onclick = async () => {
        err.hidden = true;
        const raw = input.value.trim();
        if (!/^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$/.test(raw.toLowerCase())) {
          err.hidden = false;
          err.textContent =
            "Use 1–64 lowercase letters, digits, dot, dash, or underscore; no leading or trailing separator.";
          return;
        }
        save.disabled = true;
        try {
          await api.setMailHandle(raw);
          await this.refreshStatus();
          this.announce("Mail address saved");
          this.render();
        } catch (e) {
          err.hidden = false;
          err.textContent =
            e instanceof AppError ? e.message : "Couldn't save that address.";
        } finally {
          save.disabled = false;
        }
      };
      body.append(current, label, row, err, save);
    } else if (this.settingsSection === "notifications") {
      body.innerHTML = `<h2 class="text-22">Notifications</h2><p class="muted text-13">The arrival sound plays for chats only, and only while that conversation is not on screen. Mail is silent — its unread count on the rail is the notice.</p>`;
      const soundRow = document.createElement("label");
      soundRow.className = "text-15";
      soundRow.style.display = "flex";
      soundRow.style.alignItems = "center";
      soundRow.style.gap = "var(--space-3)";
      soundRow.style.marginTop = "var(--space-4)";
      const soundCb = document.createElement("input");
      soundCb.type = "checkbox";
      soundCb.checked = chatSoundEnabled();
      soundCb.onchange = () => {
        void setChatSoundEnabled(soundCb.checked);
      };
      soundRow.append(
        soundCb,
        document.createTextNode("Play a sound when a chat message arrives"),
      );
      body.appendChild(soundRow);
      // Play the clip on demand: the arrival path depends on which pane is
      // open, so a test button is the only way to hear the output chain
      // (webview → Tauri command → rodio → device) on its own.
      const testBtn = document.createElement("button");
      testBtn.type = "button";
      testBtn.className = "btn ghost";
      testBtn.style.marginTop = "var(--space-3)";
      testBtn.textContent = "Play test sound";
      testBtn.onclick = () => {
        playChatSound();
        this.announce("Test sound requested");
      };
      body.appendChild(testBtn);
      const hint = document.createElement("p");
      hint.className = "muted text-13";
      hint.style.marginTop = "var(--space-2)";
      hint.textContent =
        "Per contact, the sound can be turned off from the chat's menu (the … button).";
      body.appendChild(hint);
    } else if (this.settingsSection === "security") {
      body.innerHTML = `<h2 class="text-22">Security</h2><p class="muted text-13">Idle auto-lock is enforced by the daemon. Suspend / sleep also locks the vault on Linux when logind is available.</p>`;
      const label = document.createElement("p");
      label.className = "muted";
      label.style.marginTop = "var(--space-4)";
      label.textContent = "Lock after inactivity";
      const sel = document.createElement("select");
      sel.className = "input-field";
      sel.style.marginTop = "var(--space-2)";
      const options: { v: number; t: string }[] = [
        { v: 5, t: "5 minutes" },
        { v: 15, t: "15 minutes" },
        { v: 30, t: "30 minutes" },
        { v: 60, t: "1 hour" },
        { v: 0, t: "Never (idle only)" },
      ];
      for (const o of options) {
        const opt = document.createElement("option");
        opt.value = String(o.v);
        opt.textContent = o.t;
        sel.appendChild(opt);
      }
      sel.value = String(getAutoLockMinutes());
      sel.onchange = () => {
        const v = Number(sel.value);
        void patchUiPrefs({ auto_lock_minutes: v }).then(() => this.render());
      };
      body.append(label, sel, this.renderChangePassphraseForm());
    } else if (this.settingsSection === "recovery") {
      body.innerHTML = `<h2 class="text-22">Recovery</h2><p class="muted text-13">Your recovery code was shown once when the vault was created or when recovery was first set up. It unlocks the vault if you forget your passphrase (with your backup file).</p>`;
      const ack = this.status?.recovery_acknowledged;
      const note = document.createElement("p");
      note.className = "text-13 muted";
      note.style.marginTop = "var(--space-3)";
      note.textContent = ack
        ? "Recovery code acknowledged. Keep your saved copy offline."
        : "You still need to save your recovery code when prompted.";
      body.appendChild(note);
    } else if (this.settingsSection === "backup") {
      body.innerHTML = `<h2 class="text-22">Backup</h2><p class="muted text-13">Download an encrypted archive of your vault. Restore by locking the app, then importing the file.</p>`;
      const exportBtn = document.createElement("button");
      exportBtn.type = "button";
      exportBtn.className = "btn-primary pressable";
      exportBtn.textContent = "Export backup";
      exportBtn.style.marginTop = "var(--space-4)";
      exportBtn.onclick = async () => {
        const r = await api.backupExport();
        if (r) await downloadBase64File(r.filename, r.data_base64);
        await this.refreshStatus();
        this.announce("Backup downloaded");
      };
      const importBtn = document.createElement("button");
      importBtn.type = "button";
      importBtn.className = "btn-secondary pressable";
      importBtn.style.marginTop = "var(--space-3)";
      importBtn.textContent = "Import backup (vault must be locked)";
      const fileIn = document.createElement("input");
      fileIn.type = "file";
      fileIn.accept = ".tar,application/x-tar";
      fileIn.hidden = true;
      importBtn.onclick = () => fileIn.click();
      fileIn.onchange = async () => {
        const f = fileIn.files?.[0];
        fileIn.value = "";
        if (!f) return;
        if (!this.status?.locked) {
          window.alert("Lock the vault before importing a backup.");
          return;
        }
        try {
          const b64 = await readFileAsBase64(f);
          await api.backupImport(b64);
          this.announce("Backup imported. Unlock with your passphrase.");
        } catch (e) {
          window.alert(e instanceof AppError ? e.message : "Import failed");
        }
      };
      body.append(exportBtn, importBtn, fileIn);
    } else if (this.settingsSection === "about") {
      body.innerHTML = `<h2 class="text-22">About</h2><p class="muted text-13">Where this client sends ciphertext, and where your vault lives. The relay value comes from the vault once it is unlocked; while locked it shows the configured default.</p>`;
      const rows: [string, string][] = [
        ["Version", this.appVersion ?? "—"],
        ["Relay", this.status?.relay ?? "—"],
        ["Relay source", this.status?.relay_source ?? "—"],
        ["Vault", this.status?.vault_dir ?? "—"],
        ["API", getApiBase()],
        [
          "TCP",
          this.status?.tcp_addr ??
            "off (Unix socket only — restart with --addr to enable)",
        ],
      ];
      const list = document.createElement("dl");
      list.className = "about-list";
      list.style.marginTop = "var(--space-4)";
      for (const [k, v] of rows) {
        const dt = document.createElement("dt");
        dt.className = "muted text-13";
        dt.textContent = k;
        const dd = document.createElement("dd");
        dd.className = "text-13";
        dd.style.wordBreak = "break-all";
        dd.textContent = v;
        list.append(dt, dd);
      }
      const hint = document.createElement("p");
      hint.className = "faint text-12";
      hint.style.marginTop = "var(--space-4)";
      hint.textContent =
        "Change the default with: hush-cli config set --relay <url>. Existing vaults keep the relay they were created with.";
      body.append(list, hint, this.renderUpdateControls());
    } else if (this.settingsSection === "drops") {
      this.renderDropsSettingsInto(body);
    } else {
      body.innerHTML = `<h2 class="text-22">${this.settingsSection}</h2><p class="muted">More options coming soon.</p>`;
    }
    pane.append(nav, body);
    return pane;
  }

  /** Two-state rotate button: the confirm states what rotation preserves. */
  private dropRotateArmed = false;
  /** The last forced drain's outcome line, shown in Settings → Drops. */
  private dropDrainNote: string | null = null;

  /**
   * Settings → Drops: publish the drop key and the page URL, rotate, force a
   * drain.
   *
   * The two strings that travel together are shown together, because that is
   * how they are published: the key says who can receive, the URL says where
   * to compose. The key is rendered as selectable text, not an input — it is
   * a value to copy, never to edit.
   */
  private renderDropsSettingsInto(body: HTMLElement): void {
    body.innerHTML = `<h2 class="text-22">Drops</h2><p class="muted text-13">An anonymous drop is a message sealed to your drop key by someone who does not use Hush — a source, a whistleblower, a stranger. They learn nothing about you; the relay cannot open what they send. Drops arrive in the Mail sidebar's Drops folder.</p>`;

    if (!this.dropInboxKey) {
      // First open of the pane: ask the daemon, then re-render.
      void api.dropsKey().then((k) => {
        this.dropInboxKey = k;
        if (this.settingsSection === "drops") this.render();
      });
      const loading = document.createElement("p");
      loading.className = "muted text-13";
      loading.textContent = "Loading…";
      body.appendChild(loading);
      return;
    }

    if (!this.dropInboxKey.exists) {
      const gen = document.createElement("button");
      gen.type = "button";
      gen.className = "btn-primary pressable";
      gen.textContent = "Generate drop key";
      gen.onclick = () => {
        gen.disabled = true;
        void api
          .dropsKeyCreate()
          .then((k) => {
            this.dropInboxKey = k;
            this.announce("Drop key generated");
            this.render();
          })
          .catch((e) => {
            this.announce(
              e instanceof AppError ? e.message : "Could not generate the drop key.",
            );
            gen.disabled = false;
          });
      };
      body.appendChild(gen);
      return;
    }

    const pk = this.dropInboxKey.public_key ?? "";
    const keyLabel = document.createElement("p");
    keyLabel.className = "muted text-13";
    keyLabel.style.marginTop = "var(--space-4)";
    keyLabel.textContent = "Your drop key (publish this):";
    const keyRow = document.createElement("div");
    keyRow.className = "drop-key-row";
    const keyText = document.createElement("code");
    keyText.className = "drop-key-value text-12";
    keyText.textContent = pk;
    keyText.title = pk;
    const copyKey = document.createElement("button");
    copyKey.type = "button";
    copyKey.className = "btn-secondary pressable";
    copyKey.textContent = "Copy";
    copyKey.onclick = () =>
      void navigator.clipboard
        .writeText(pk)
        .then(() => this.announce("Drop key copied"))
        .catch(() => this.announce(pk));
    keyRow.append(keyText, copyKey);
    body.appendChild(keyLabel);
    body.appendChild(keyRow);

    const pageUrl = this.dropInboxKey.page_url ?? "";
    const urlLabel = document.createElement("p");
    urlLabel.className = "muted text-13";
    urlLabel.style.marginTop = "var(--space-3)";
    urlLabel.textContent = "The page outsiders open to send:";
    const urlRow = document.createElement("div");
    urlRow.className = "drop-key-row";
    const urlText = document.createElement("code");
    urlText.className = "drop-key-value text-12";
    urlText.textContent = pageUrl;
    const copyUrl = document.createElement("button");
    copyUrl.type = "button";
    copyUrl.className = "btn-secondary pressable";
    copyUrl.textContent = "Copy";
    copyUrl.onclick = () =>
      void navigator.clipboard
        .writeText(pageUrl)
        .then(() => this.announce("Drop page URL copied"))
        .catch(() => this.announce(pageUrl));
    urlRow.append(urlText, copyUrl);
    body.appendChild(urlLabel);
    body.appendChild(urlRow);

    const publishHint = document.createElement("p");
    publishHint.className = "faint text-12";
    publishHint.style.marginTop = "var(--space-3)";
    publishHint.textContent =
      "Publish both of these together — the key alone has nowhere to point. Put them anywhere you are already reachable: a profile page, a forum bio, a contact section.";
    body.appendChild(publishHint);

    // Rotate, in two steps: the confirm says what rotation does and does not
    // lose, because that asymmetry is the whole point of the design.
    const rotate = document.createElement("button");
    rotate.type = "button";
    rotate.className = this.dropRotateArmed ? "btn-primary pressable" : "btn-secondary pressable";
    rotate.style.marginTop = "var(--space-5)";
    rotate.textContent = this.dropRotateArmed
      ? "Confirm rotation"
      : "Rotate drop key";
    rotate.onclick = () => {
      if (!this.dropRotateArmed) {
        this.dropRotateArmed = true;
        this.render();
        return;
      }
      rotate.disabled = true;
      void api
        .dropsKeyRotate()
        .then((k) => {
          this.dropInboxKey = k;
          this.dropRotateArmed = false;
          this.announce("Drop key rotated");
          this.render();
        })
        .catch((e) => {
          this.announce(
            e instanceof AppError ? e.message : "Could not rotate the drop key.",
          );
          this.dropRotateArmed = false;
          this.render();
        });
    };
    body.appendChild(rotate);
    if (this.dropRotateArmed) {
      const warn = document.createElement("p");
      warn.className = "faint text-12";
      warn.style.marginTop = "var(--space-2)";
      warn.textContent =
        "The new key takes effect immediately. Drops already sealed to the old key still arrive until they expire at the relay — publish the new key where you published the old one.";
      body.appendChild(warn);
    }

    // A forced drain, for the moment a member publishes a key and wants to
    // see it work without waiting for the next poll.
    const drain = document.createElement("button");
    drain.type = "button";
    drain.className = "btn-secondary pressable";
    drain.style.marginTop = "var(--space-3)";
    drain.textContent = "Check for drops now";
    drain.onclick = () => {
      drain.disabled = true;
      void api
        .dropsDrain()
        .then((r) => {
          const report = r ?? { hit_slots: [], remaining: 0 };
          this.dropDrainNote = report.hit_slots.length
            ? `Delivered ${report.hit_slots.length} drop${report.hit_slots.length === 1 ? "" : "s"} to the Drops folder.`
            : report.remaining
              ? `${report.remaining} drop${report.remaining === 1 ? "" : "s"} could not be opened; left in place.`
              : "No drops waiting.";
          this.render();
        })
        .catch((e) => {
          this.dropDrainNote =
            e instanceof AppError ? e.message : "Could not reach the relay.";
          this.render();
        });
    };
    body.appendChild(drain);
    if (this.dropDrainNote) {
      const note = document.createElement("p");
      note.className = "muted text-13";
      note.style.marginTop = "var(--space-2)";
      note.textContent = this.dropDrainNote;
      body.appendChild(note);
    }
  }

  private renderPalette(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    back.onclick = (e) => {
      if (e.target === back) {
        this.paletteOpen = false;
        this.render();
      }
    };
    const modal = document.createElement("div");
    modal.className = "modal palette";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", "Command palette");
    const inp = document.createElement("input");
    inp.placeholder = "Type a command or contact…";
    const actions: { label: string; run: () => void }[] = [
      ...this.contacts.map((c) => ({
        label: `Open ${displayNick(c.nickname)}`,
        run: () => {
          this.selectedNick = c.nickname;
          this.rail = "chats";
        },
      })),
      {
        label: "Add a trust",
        run: () => {
          this.addFriendOpen = true;
        },
      },
      {
        label: "Lock vault",
        run: () => void this.lock(),
      },
      {
        label: "Toggle theme",
        run: () => {
          void setThemeMode(getThemeMode() === "dark" ? "light" : "dark").then(
            () => this.render(),
          );
        },
      },
    ];
    const list = document.createElement("div");
    const renderList = () => {
      list.innerHTML = "";
      const q = inp.value.toLowerCase();
      actions
        .filter((a) => a.label.toLowerCase().includes(q))
        .forEach((a) => {
          const btn = document.createElement("button");
          btn.className = "palette-item pressable";
          btn.textContent = a.label;
          btn.onclick = () => {
            a.run();
            this.paletteOpen = false;
            this.render();
          };
          list.appendChild(btn);
        });
    };
    inp.oninput = renderList;
    renderList();
    modal.append(inp, list);
    back.appendChild(modal);
    setTimeout(() => inp.focus(), 0);
    return back;
  }

  private renderAddFriend(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    back.onclick = (e) => {
      if (e.target === back) {
        this.addFriendOpen = false;
        this.render();
      }
    };
    const modal = document.createElement("div");
    modal.className = "modal add-trust-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", "Add a trust");
    const title = document.createElement("h2");
    title.className = "text-22";
    title.textContent = "Add a trust";
    title.style.marginBottom = "var(--space-3)";
    let tab: "show" | "add" = "show";
    // The two tabs are the two halves of one job, so they are a real tablist
    // rather than two buttons that happen to swap a panel.
    const tabs = document.createElement("div");
    tabs.className = "tabs";
    tabs.setAttribute("role", "tablist");
    tabs.setAttribute("aria-label", "How to add a trust");
    const showTab = document.createElement("button");
    showTab.type = "button";
    showTab.className = "tab active";
    showTab.id = "add-trust-tab-show";
    showTab.textContent = "Show my invite";
    showTab.setAttribute("role", "tab");
    showTab.setAttribute("aria-selected", "true");
    showTab.setAttribute("aria-controls", "add-trust-panel");
    const addTab = document.createElement("button");
    addTab.type = "button";
    addTab.className = "tab";
    addTab.id = "add-trust-tab-add";
    addTab.textContent = "Add with invite";
    addTab.setAttribute("role", "tab");
    addTab.setAttribute("aria-selected", "false");
    addTab.setAttribute("aria-controls", "add-trust-panel");
    const body = document.createElement("div");
    body.id = "add-trust-panel";
    body.setAttribute("role", "tabpanel");
    const renderTab = async () => {
      body.replaceChildren();
      body.setAttribute("aria-labelledby", tab === "show" ? showTab.id : addTab.id);
      showTab.classList.toggle("active", tab === "show");
      addTab.classList.toggle("active", tab === "add");
      showTab.setAttribute("aria-selected", String(tab === "show"));
      addTab.setAttribute("aria-selected", String(tab === "add"));
      if (tab === "show") {
        const card = document.createElement("div");
        card.className = "key-card";
        const inner = document.createElement("div");
        inner.className = "key-card-inner";
        try {
          const inv = await api.invite();
          if (!inv?.invite) throw new Error("empty invite");
          // The invite carries a Kyber1024 prekey, so it encodes to ~9 kB of
          // base64 — well past the ~3 kB a QR code holds at its lowest error
          // correction. A QR is therefore never drawn for it: the code itself is
          // the artefact, shown in full so it can be read or copied by hand.
          const label = document.createElement("p");
          label.className = "text-13 muted";
          label.style.marginBottom = "var(--space-2)";
          label.textContent = "Your invitation code";
          const code = document.createElement("textarea");
          code.className = "input-field invite-code";
          code.readOnly = true;
          code.rows = 4;
          code.value = inv.invite;
          code.setAttribute("aria-label", "Your invitation code");
          // Selecting on focus is the only practical way to grab a string this
          // long; the copy button is the fast path.
          code.onfocus = () => code.select();
          const copy = document.createElement("button");
          copy.className = "btn-secondary pressable";
          copy.textContent = "Copy invitation code";
          copy.style.marginTop = "12px";
          copy.onclick = () => {
            navigator.clipboard.writeText(inv.invite).then(
              () => this.announce("Invitation code copied."),
              () =>
                this.announce("Copy failed. Select the code and copy it by hand."),
            );
          };
          const note = document.createElement("p");
          note.className = "text-13 muted";
          note.textContent =
            "Anyone with this code can start a chat with you. Single use.";
          inner.append(label, code, copy, note);
        } catch {
          inner.textContent = "Couldn't load invite";
        }
        card.appendChild(inner);
        body.appendChild(card);
      } else {
        // Both fields carry a label: the placeholder is an example, not a name.
        const nickField = document.createElement("label");
        nickField.className = "conclave-field";
        const nickText = document.createElement("span");
        nickText.className = "text-13";
        nickText.textContent = "Their name";
        const nick = document.createElement("input");
        nick.type = "text";
        nick.className = "input-field";
        nick.placeholder = "e.g. Ada";
        nick.autocomplete = "off";
        nick.spellcheck = false;
        nickField.append(nickText, nick);

        const invField = document.createElement("label");
        invField.className = "conclave-field";
        const invText = document.createElement("span");
        invText.className = "text-13";
        invText.textContent = "Their invitation code";
        const inv = document.createElement("textarea");
        inv.className = "input-field invite-code";
        inv.rows = 3;
        inv.placeholder = "Paste the code they sent you";
        inv.spellcheck = false;
        invField.append(invText, inv);

        const err = document.createElement("p");
        err.className = "field-error text-13";
        err.hidden = true;

        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "btn-primary pressable";
        btn.textContent = "Add contact";
        btn.style.marginTop = "var(--space-4)";
        // Both halves are required, so the button says so rather than letting a
        // round trip fail on a field that was never filled in.
        const syncBtn = () => {
          btn.disabled = !nick.value.trim() || !inv.value.trim();
        };
        nick.oninput = syncBtn;
        inv.oninput = () => {
          syncBtn();
          // The reason belonged to the last attempt; a new code gets a fresh one.
          if (!err.hidden) {
            err.hidden = true;
            err.textContent = "";
          }
        };
        btn.onclick = async () => {
          err.hidden = true;
          try {
            await api.addContact(nick.value.trim(), inv.value.trim());
            this.contacts = (await api.contacts()) ?? [];
            setKnownContacts(this.contacts.map((c) => c.nickname));
            this.addFriendOpen = false;
            this.announce(`Added ${nick.value.trim()}.`);
            this.render();
          } catch (e) {
            err.hidden = false;
            err.textContent =
              e instanceof AppError ? e.message : "Couldn't add contact";
          }
        };
        syncBtn();
        const hint = document.createElement("p");
        hint.className = "text-13 muted";
        hint.textContent = "Names live only on your device.";
        body.append(nickField, invField, err, btn, hint);
      }
    };
    // A tablist is expected to answer the arrow keys.
    tabs.onkeydown = (e) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      e.preventDefault();
      tab = tab === "show" ? "add" : "show";
      void renderTab();
      (tab === "show" ? showTab : addTab).focus();
    };
    showTab.onclick = () => {
      tab = "show";
      void renderTab();
    };
    addTab.onclick = () => {
      tab = "add";
      void renderTab();
    };
    void renderTab();
    tabs.append(showTab, addTab);
    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn-secondary pressable";
    close.textContent = "Done";
    close.style.marginTop = "16px";
    close.onclick = () => {
      this.addFriendOpen = false;
      this.render();
    };
    modal.append(title, tabs, body, close);
    back.appendChild(modal);
    // Focus once the node is in the document, so the dialog is keyboard
    // reachable without tabbing down from the top of the app.
    queueMicrotask(() => showTab.focus());
    return back;
  }

  /**
   * The contact card reached from a conversation's overflow menu. It is only
   * about identity: the name this vault shows for the person. Actions on the
   * contact (mute, delete) live in the menu that opened it, so each is decided
   * in one place.
   */
  private renderProfile(): HTMLElement {
    const nick = this.profileNick!;
    const c = this.contacts.find((x) => x.nickname === nick);
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    const title = document.createElement("h2");
    title.className = "text-22";
    title.style.marginBottom = "var(--space-3)";
    title.textContent = "Contact";
    modal.appendChild(title);
    if (c) modal.appendChild(createAvatarEl(c.protocol_name, "lg", false, displayNick(nick)));
    const name = document.createElement("input");
    name.className = "input-field";
    name.value = displayNick(nick);
    name.setAttribute("aria-label", "Display name");
    name.style.marginTop = "12px";
    const hint = document.createElement("p");
    hint.className = "text-13 muted";
    hint.textContent = "The name this vault shows. Their own name is unchanged.";
    const actions = document.createElement("div");
    actions.className = "modal-actions";
    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn-secondary pressable";
    close.textContent = "Cancel";
    close.onclick = () => {
      this.profileNick = null;
      this.render();
    };
    const save = document.createElement("button");
    save.type = "button";
    save.className = "btn-primary pressable";
    save.textContent = "Save";
    save.onclick = () => {
      void setNickOverride(nick, name.value).then(() => {
        this.profileNick = null;
        this.render();
      });
    };
    actions.append(close, save);
    modal.append(name, hint, actions);
    back.appendChild(modal);
    // Reached to rename, so the field starts focused and selected; the select
    // makes the suggestion the user types over the old name in one keystroke.
    queueMicrotask(() => {
      name.focus();
      name.select();
    });
    return back;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
