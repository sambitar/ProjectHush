export const MAX_FILE_BYTES = 256 * 1024;
export const RELAY_TTL_MS = 7 * 24 * 60 * 60 * 1000;
export const AUTO_LOCK_MS = 15 * 60 * 1000;
export const RECV_POLL_MS = 4000;

/**
 * Trailing delay that coalesces background renders. Polls and store
 * subscriptions often fire in bursts (a presence flip, then a message, then
 * a mailbox refresh); one rebuild after the burst beats one per event.
 */
export const RENDER_BATCH_MS = 100;

/**
 * How long after launch the silent update probe waits. Long enough that the
 * first paint, the status call, and the mailbox load are done and the check
 * never competes with them for the network or the main thread.
 */
export const STARTUP_UPDATE_DELAY_MS = 5000;
