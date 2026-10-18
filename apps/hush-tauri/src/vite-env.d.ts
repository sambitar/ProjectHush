interface Window {
  __HUSH_API__?: string;
  /** Set by the Tauri shell; absent when the UI runs in a plain browser. */
  __HUSH_TRANSPORT__?:
    | { kind: "unix"; path: string }
    | { kind: "tcp"; base: string };
  /**
   * How many times the shell has been rebuilt, counted by `App.render()`.
   *
   * The smoothness checks read it from the webview console: with no input and
   * no traffic it must not move, and a presence flip while another rail is open
   * must not move it either (docs/tasks/app-smoothness.md, "How we will know it
   * worked").
   */
  __HUSH_RENDER_COUNT__?: number;
}

declare module "*.wav" {
  const src: string;
  export default src;
}
