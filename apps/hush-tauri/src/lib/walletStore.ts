import { api } from "./api/daemon";
import type {
  WalletChain,
  WalletChainDto,
  WalletNetworkMode,
  WalletSendDto,
  WalletStateDto,
} from "./api/types";

/**
 * The Wallet pane's client-side store.
 *
 * All the real work — key derivation, sealing the phrase into the vault, the
 * electrum scan, the RPC calls, signing — happens in the daemon. This store is
 * only a small cache plus the mutation wrappers, so the pane can render from
 * one object and re-render on change.
 *
 * Polling is deliberately slow: reading the wallet state does not touch the
 * network (a sync is an explicit action), so the only thing a poll can pick up
 * is a mode change made elsewhere — the relay switch in Settings. Twenty
 * seconds is fast enough for that and costs nothing while it waits.
 */

const OPEN_POLL_MS = 20000;

/** The two chains the pane shows, in the order it shows them. */
export const CHAINS: WalletChain[] = ["bitcoin", "ethereum"];

/** A chain with no wallet behind it, used before the daemon has answered. */
function emptyChain(chain: WalletChain): WalletChainDto {
  return {
    chain,
    available: true,
    address: null,
    unit: chain === "bitcoin" ? "sat" : "wei",
    confirmed: "0",
    pending: "0",
  };
}

const EMPTY: WalletStateDto = {
  exists: false,
  network: "mainnet",
  mode: "tor",
  mode_locked: false,
  label: null,
  created_at: 0,
  chains: [emptyChain("bitcoin"), emptyChain("ethereum")],
  tokens: [],
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

/** The named chain's entry. Falls back to an empty one, so a caller that runs
 * before the first load still renders something shaped correctly. */
export function chainState(
  state: WalletStateDto,
  chain: WalletChain,
): WalletChainDto {
  return state.chains.find((c) => c.chain === chain) ?? emptyChain(chain);
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
export async function createWallet(
  network: string,
  ethereumNetwork: string,
  mode: WalletNetworkMode,
): Promise<string> {
  const result = await api.walletCreate(network, ethereumNetwork, mode);
  if (!result)
    throw new Error("The wallet was created but the daemon sent no state.");
  apply(result.state);
  return result.mnemonic;
}

/** Restore from an existing phrase. The daemon validates it before storing. */
export async function restoreWallet(
  mnemonic: string,
  network: string,
  ethereumNetwork: string,
  mode: WalletNetworkMode,
): Promise<WalletStateDto> {
  return apply(
    await api.walletRestore(mnemonic, network, ethereumNetwork, mode),
  );
}

/**
 * Reveal a fresh receive address, so the shown one is never reused.
 *
 * Bitcoin only: an Ethereum account is one address for the life of the wallet,
 * and the daemon returns it unchanged rather than pretending to rotate.
 */
export async function newReceiveAddress(chain: WalletChain): Promise<string> {
  const result = await api.walletAddress(chain);
  if (!result) throw new Error("The daemon sent no address.");
  // The keychain has already advanced, so the address is real whether or not
  // the state read that follows it succeeds. A failed read must not be
  // reported as a failed reveal: it would invite a second click and burn
  // another address for nothing.
  await refreshWallet().catch(() => {});
  return result.address;
}

/** Read the chain's balances again — an electrum scan, or an RPC read. */
export async function syncWallet(chain: WalletChain): Promise<WalletStateDto> {
  return apply(await api.walletSync(chain));
}

/** Send value, or a token. `amount` is base units, as a decimal string. */
export async function sendPayment(send: {
  chain: WalletChain;
  to: string;
  amount: string;
  feeRate: number | null;
  token?: string | null;
}): Promise<WalletSendDto> {
  const result = await api.walletSend({
    chain: send.chain,
    to: send.to,
    amount: send.amount,
    fee_rate: send.feeRate,
    token: send.token ?? null,
  });
  if (!result) throw new Error("The daemon sent no transaction id.");
  // Same reasoning as the address, with more at stake: the money has already
  // moved, so a failed state read must never surface as "the payment could not
  // be sent". That would invite the user to send it a second time.
  await refreshWallet().catch(() => {});
  return result;
}

/** Track an ERC-20 token by contract address. */
export async function addToken(address: string): Promise<WalletStateDto> {
  return apply(await api.walletTokenAdd(address));
}

/** Stop tracking a token. */
export async function removeToken(address: string): Promise<WalletStateDto> {
  return apply(await api.walletTokenRemove(address));
}

/** Switch the wallet's network. Refused by the daemon while the relay is on Tor. */
export async function setWalletMode(
  mode: WalletNetworkMode,
): Promise<WalletStateDto> {
  return apply(await api.walletSetMode(mode));
}

/**
 * Report the shell's Tor SOCKS port to the daemon, for the wallet.
 *
 * The wallet's Tor is the shell's Tor — the same process the relay and the Drop
 * window use — so this is an announcement, not a setting. Until one arrives a
 * Tor-mode wallet has no route, and says so rather than going direct.
 */
export async function setWalletProxy(
  socks: string | null,
): Promise<WalletStateDto> {
  return apply(await api.walletSetProxy(socks));
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

// --- Amounts ------------------------------------------------------------------
//
// Every amount on the wire is a decimal string in base units, and every
// conversion here is integer arithmetic on that string. A float would round:
// 0.1 ether is not representable in binary, and one wei of rounding is one wei
// of someone's money.

/**
 * A base-unit amount as a human number, truncated (never rounded up) to
 * `maxFrac` decimal places.
 *
 * Truncation matters: rounding a balance up shows money the user does not
 * have, and rounding a fee down shows less than they will pay.
 */
export function formatUnits(value: string, decimals: number, maxFrac = 6): string {
  if (!/^\d+$/.test(value)) return "0";
  const padded = value.padStart(decimals + 1, "0");
  const whole = padded.slice(0, padded.length - decimals) || "0";
  const frac = decimals === 0 ? "" : padded.slice(padded.length - decimals);
  const shown = frac.slice(0, maxFrac).replace(/0+$/, "");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  if (shown) return `${grouped}.${shown}`;
  // A balance too small to show at this precision is not zero; saying "0"
  // would be a lie the user cannot see through.
  return value !== "0" && whole === "0" ? `<0.${"0".repeat(maxFrac - 1)}1` : grouped;
}

/**
 * A human amount in the display unit as base units, or null when it cannot be
 * represented exactly at that precision.
 *
 * Exact or nothing: a user who types 0.0000000001 ether has typed less than a
 * wei, and sending "0" instead would be sending nothing while saying otherwise.
 */
export function parseUnits(value: string, decimals: number): string | null {
  const trimmed = value.trim();
  if (!/^\d+(\.\d*)?$|^\.\d+$/.test(trimmed)) return null;
  const [whole = "", frac = ""] = trimmed.split(".");
  if (frac.length > decimals) return null;
  const digits = `${whole || "0"}${frac.padEnd(decimals, "0")}`.replace(/^0+(?=\d)/, "");
  return digits === "" ? "0" : digits;
}

/** Satoshi as BTC. */
export function formatBtc(sats: string): string {
  return formatUnits(sats, 8);
}

/** Wei as ETH, shown to six places: the seventh is noise for a human. */
export function formatEth(wei: string): string {
  return formatUnits(wei, 18);
}

/** A base-unit amount with its unit name, e.g. "1,250,000 sat". */
export function formatBase(value: string, unit: string): string {
  if (!/^\d+$/.test(value)) return `0 ${unit}`;
  return `${value.replace(/\B(?=(\d{3})+(?!\d))/g, ",")} ${unit}`;
}

/** A token balance in its own units, e.g. "12.5 USDC". */
export function formatToken(balance: string, decimals: number, symbol: string): string {
  return `${formatUnits(balance, decimals)} ${symbol}`;
}

/** The default fee rate the daemon assumes when the UI sends none. */
export const DEFAULT_FEE_RATE = 2;

/** The Ethereum network the UI creates wallets on. Mainnet, like Bitcoin's. */
export const DEFAULT_ETHEREUM_NETWORK = "mainnet";
