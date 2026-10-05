import { hueFromKey } from "../../lib/hash";

const photoKey = (protocol: string) => `hush_avatar_${protocol}`;

export function getLocalPhoto(protocol: string): string | null {
  return localStorage.getItem(photoKey(protocol));
}

export function setLocalPhoto(protocol: string, dataUrl: string): void {
  localStorage.setItem(photoKey(protocol), dataUrl);
}

export function createAvatarEl(
  protocolName: string,
  size: "sm" | "lg" = "sm",
  ring = false,
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
  return wrap;
}
