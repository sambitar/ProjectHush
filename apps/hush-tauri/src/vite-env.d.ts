interface Window {
  __HUSH_API__?: string;
  /** Set by the Tauri shell; absent when the UI runs in a plain browser. */
  __HUSH_TRANSPORT__?:
    | { kind: "unix"; path: string }
    | { kind: "tcp"; base: string };
}

declare module "*.wav" {
  const src: string;
  export default src;
}
