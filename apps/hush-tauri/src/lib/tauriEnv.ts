/** True when the UI runs inside the Hush Tauri shell (not plain browser). */
export function isTauriShell(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}
