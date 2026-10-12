import { patchUiPrefs, uiPrefsSnapshot } from "../../lib/api/uiPrefs";
import { hueFromKey } from "../../lib/hash";

export function getLocalPhoto(protocol: string): string | null {
  return uiPrefsSnapshot().avatars[protocol] ?? null;
}

export async function setLocalPhoto(
  protocol: string,
  dataUrl: string,
): Promise<void> {
  const p = uiPrefsSnapshot();
  await patchUiPrefs({
    avatars: { ...p.avatars, [protocol]: dataUrl },
  });
}

export function createAvatarEl(
  protocolName: string,
  size: "sm" | "lg" = "sm",
  ring = false,
  label = "",
): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = `avatar ${size === "lg" ? "lg" : ""}${ring ? " avatar-ring" : ""}`;

  const photo = getLocalPhoto(protocolName);
  if (photo) {
    const img = document.createElement("img");
    img.src = photo;
    img.alt = "";
    img.style.width = "100%";
    img.style.height = "100%";
    img.style.objectFit = "cover";
    wrap.appendChild(img);
    return wrap;
  }

  const h = hueFromKey(protocolName);
  const h2 = (h + 40) % 360;
  const h3 = (h + 80) % 360;
  wrap.style.background = `linear-gradient(135deg, hsl(${h},55%,62%), hsl(${h2},50%,55%), hsl(${h3},45%,48%))`;

  // The monogram is the fallback when no photo is set. A bare gradient is
  // decoration — it cannot be told apart in a list; initials make the disc
  // scannable without needing an uploaded image.
  const initials = initialsFor(label);
  if (initials) {
    const span = document.createElement("span");
    span.className = "avatar-initials";
    span.textContent = initials;
    wrap.appendChild(span);
  }
  return wrap;
}

/**
 * Up to two initials for a display name: the first and last word. Takes the
 * first grapheme rather than the first code unit, so a leading emoji or any
 * non-BMP character is not sliced in half.
 */
export function initialsFor(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "";
  const words = parts.length >= 2 ? [parts[0], parts[parts.length - 1]] : [parts[0]];
  return words.map((w) => Array.from(w)[0]?.toUpperCase() ?? "").join("");
}

/** Avatar with live presence ring (green when online). */
export function avatarWithPresence(
  protocolName: string,
  online: boolean,
  size: "sm" | "lg" = "sm",
  ring = false,
  label = "",
): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = `avatar-presence-wrap${online ? " is-online" : ""}`;
  wrap.appendChild(createAvatarEl(protocolName, size, ring, label));
  const dot = document.createElement("span");
  dot.className = "presence-dot";
  dot.setAttribute("aria-hidden", "true");
  wrap.appendChild(dot);
  return wrap;
}
