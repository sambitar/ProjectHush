import { api } from "./api/daemon";
import type {
  WalletNetworkMode,
  WalletSendDto,
  WalletStateDto,
} from "./api/types";

/**
 * The Wallet pane's client-side store.
 *
 * All the real work — key derivation, sealing the phrase into the vault,
 * electrum sync, signing — happens in the daemon. This store is only a small
 * cache plus the mutation wrappers, so the pane can render from one object and
 * re-render on change.
 *
 * Polling is deliberately slow: reading the wallet state does not touch the
 * network (a sync is an explicit action), so the only thing a poll can pick up
 * is a mode change made elsewhere — the relay switch in Settings. Twenty
 * seconds is fast enough for that and costs nothing while it waits.
 */

const OPEN_POLL_MS = 20000;

const EMPTY: WalletStateDto = {
  exists: false,
  network: "mainnet",
  mode: "tor",
  mode_locked: false,
  label: null,
  address: null,
  confirmed_sat: 0,
  pending_sat: 0,
  created_at: 0,
};

type Listener = (state: WalletStateDto) => void;

let cached: WalletStateDto | null = null;
let timer: number | null = null;
let paneOpen = false;
let inFlight = false;
/**
 * Whether the daemon has ever answered. Until it has, the pane must not draw
 * the "no wallet yet" form: it cannot tell an empty vault from an unreachable
 * one, and telling a user with a funded wallet to create one would be a lie.
 */
let loaded = false;
/** Why the first load failed, when it did. Cleared by the first success. */
let loadError: string | null = null;
const listeners = new Set<Listener>();

export function getWalletState(): WalletStateDto {
  return cached ?? EMPTY;
}

export function isWalletLoaded(): boolean {
  return loaded;
}

/** Why the wallet state could not be read, if it could not. */
export function getWalletLoadError(): string | null {
  return loadError;
}

export function subscribeWallet(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function emit(): void {
  const state = getWalletState();
  for (const fn of listeners) fn(state);
}

/**
 * Fold a fresh state in. Returns true when the rendered result would change,
 * so a poll that finds nothing new does not re-render the pane out from under
 * the user (which would also throw away its scroll position).
 */
function setState(next: WalletStateDto): boolean {
  const changed = JSON.stringify(cached) !== JSON.stringify(next);
  cached = next;
  loaded = true;
  loadError = null;
  if (changed) emit();
  return changed;
}

function apply(next: WalletStateDto | null): WalletStateDto {
  if (next) setState(next);
  schedule();
  return getWalletState();
}

function schedule(): void {
  if (timer !== null) {
    window.clearTimeout(timer);
    timer = null;
  }
  if (!paneOpen) return;
  timer = window.setTimeout(() => void poll(), OPEN_POLL_MS);
}

async function poll(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const state = await api.walletState();
    if (state) setState(state);
  } catch (e) {
    // A background blip keeps the last good state and retries. Only the first
    // load has nothing to keep, and that one has to be said out loud.
    if (!loaded) {
      loadError =
        e instanceof Error ? e.message : "The wallet state could not be read.";
      emit();
    }
  } finally {
    inFlight = false;
    schedule();
  }
}

/** Fetch once, now — used when the pane opens and after every mutation. */
export async function refreshWallet(): Promise<WalletStateDto> {
  try {
    return apply(await api.walletState());
  } catch (e) {
    // Unlike a background poll, an explicit refresh must surface the reason:
    // a TCP-only daemon answers 404 here, and that is worth saying out loud.
    if (!loaded) {
      loadError =
        e instanceof Error ? e.message : "The wallet state could not be read.";
      emit();
    }
    schedule();
    throw e;
  }
}

/** Tell the store whether the pane is on screen, so it can poll or stop. */
export function setWalletPaneOpen(open: boolean): void {
  paneOpen = open;
  if (open) {
    void poll();
  } else {
    schedule();
  }
}

/** Clear everything on lock; a fresh unlock reloads from the daemon. */
export function clearWalletStore(): void {
  if (timer !== null) window.clearTimeout(timer);
  timer = null;
  cached = null;
  loaded = false;
  loadError = null;
}

// --- Mutations: each applies the daemon's new state at once. ---

/** Create a wallet. The returned phrase is the only time it is ever shown. */
export async function createWallet(network: string): Promise<string> {
  const result = await api.walletCreate(network);
  if (!result) throw new Error("The wallet was created but the daemon sent no state.");
  apply(result.state);
  return result.mnemonic;
}

/** Restore from an existing phrase. The daemon validates it before storing. */
export async function restoreWallet(
  mnemonic: string,
  network: string,
): Promise<WalletStateDto> {
  return apply(await api.walletRestore(mnemonic, network));
}

/** Reveal a fresh receive address, so the shown one is never reused. */
export async function newReceiveAddress(): Promise<string> {
  const result = await api.walletAddress();
  if (!result) throw new Error("The daemon sent no address.");
  // The keychain has already advanced, so the address is real whether or not
  // the state read that follows it succeeds. A failed read must not be
  // reported as a failed reveal: it would invite a second click and burn
  // another address for nothing.
  await refreshWallet().catch(() => {});
  return result.address;
}

/** Full-scan the chain and refresh the balance. */
export async function syncWallet(): Promise<WalletStateDto> {
  return apply(await api.walletSync());
}

export async function sendPayment(
  to: string,
  amountSat: number,
  feeRate: number | null,
): Promise<WalletSendDto> {
  const result = await api.walletSend(to, amountSat, feeRate);
  if (!result) throw new Error("The daemon sent no transaction id.");
  // Same reasoning as the address, with more at stake: the money has already
  // moved, so a failed state read must never surface as "the payment could not
  // be sent". That would invite the user to send it a second time.
  await refreshWallet().catch(() => {});
  return result;
}

/** Switch the wallet's network. Refused by the daemon while the relay is on Tor. */
export async function setWalletMode(
  mode: WalletNetworkMode,
): Promise<WalletStateDto> {
  return apply(await api.walletSetMode(mode));
}

/** Forget the wallet: the sealed sidecar is overwritten with an empty state. */
export async function wipeWallet(): Promise<WalletStateDto> {
  await api.walletWipe();
  const previous = getWalletState();
  try {
    return apply(await api.walletState());
  } catch {
    // The wipe already happened, so the wallet is gone whether or not the read
    // that follows it works. Show that, keeping the relay's lock, rather than
    // leaving a wallet on screen that no longer exists.
    return apply({
      ...EMPTY,
      mode: previous.mode,
      mode_locked: previous.mode_locked,
    });
  }
}

// --- Formatting ---------------------------------------------------------------

const SATS_PER_BTC = 100_000_000;

/**
 * Satoshi as BTC, trimmed to a readable number of decimals.
 *
 * Bitcoin amounts are small enough that a fixed 8 decimals is noise: 0.00012345
 * reads better than 0.00012345 (which is the same) but 1.00000000 does not read
 * better than 1. Trailing zeros are dropped, and the value is never rounded up
 * into a different amount of money.
 */
export function formatBtc(sats: number): string {
  if (!Number.isFinite(sats)) return "0";
  const btc = sats / SATS_PER_BTC;
  const fixed = btc.toFixed(8);
  return fixed.replace(/\.?0+$/, "") || "0";
}

/** A satoshi count with thousands separators, e.g. "1,250,000 sat". */
export function formatSats(sats: number): string {
  return `${sats.toLocaleString()} sat`;
}

/** The default fee rate the daemon assumes when the UI sends none. */
export const DEFAULT_FEE_RATE = 2;
