import { icon, type IconName } from "../../lib/icons";
import {
  bodyToDisplayHtml,
  htmlToText,
  isBlankHtml,
  sanitizeHtml,
} from "../../lib/richText";

/**
 * A dependency-free rich-text editor for the mail composer.
 *
 * It is a `contenteditable` surface with a fixed toolbar. The formatting
 * commands are `document.execCommand`, which is deprecated but is the only
 * formatting API every Tauri WebView (WebKitGTK, WebView2, WKWebView) ships
 * without adding an editor dependency. The alternative — re-implementing
 * selection and range formatting — is a lot of code for the same result.
 *
 * The security boundary is not here: the surface may transiently hold whatever
 * a paste put in it. `getHtml()` sanitizes on the way out, and the renderer
 * sanitizes on the way in, so an unsanitized value never reaches the log or the
 * screen.
 */

interface ToolbarButton {
  name: string;
  iconName: IconName;
  title: string;
  /** execCommand name. */
  command: string;
  /** execCommand argument, for the block-level commands. */
  value?: string;
}

const TOOLBAR: ToolbarButton[] = [
  { name: "bold", iconName: "bold", title: "Bold", command: "bold" },
  { name: "italic", iconName: "italic", title: "Italic", command: "italic" },
  { name: "underline", iconName: "underline", title: "Underline", command: "underline" },
  { name: "strike", iconName: "strike", title: "Strikethrough", command: "strikeThrough" },
  { name: "h2", iconName: "heading", title: "Heading", command: "formatBlock", value: "h2" },
  { name: "ul", iconName: "list", title: "Bulleted list", command: "insertUnorderedList" },
  { name: "ol", iconName: "listOrdered", title: "Numbered list", command: "insertOrderedList" },
  { name: "quote", iconName: "quote", title: "Quote", command: "formatBlock", value: "blockquote" },
  { name: "code", iconName: "code", title: "Code block", command: "formatBlock", value: "pre" },
  { name: "link", iconName: "link", title: "Link", command: "createLink" },
  { name: "clear", iconName: "clearFormat", title: "Clear formatting", command: "removeFormat" },
];

export interface RichTextEditorOptions {
  /** Initial body: HTML, or plain text if it contains no tags. */
  value?: string;
  placeholder?: string;
  label?: string;
  /** Fired on every edit, so a re-render can restore what was typed. */
  onChange?: () => void;
}

export class RichTextEditor {
  readonly element: HTMLElement;
  private readonly surface: HTMLDivElement;
  private readonly buttons = new Map<string, HTMLButtonElement>();
  private readonly onChange?: () => void;
  private readonly onSelectionChange = () => this.refreshActive();

  constructor(opts: RichTextEditorOptions = {}) {
    this.onChange = opts.onChange;
    this.element = document.createElement("div");
    this.element.className = "rich-editor";

    const toolbar = document.createElement("div");
    toolbar.className = "rich-editor-toolbar";
    toolbar.setAttribute("role", "toolbar");
    toolbar.setAttribute("aria-label", "Formatting");
    for (const button of TOOLBAR) {
      toolbar.appendChild(this.buildButton(button));
    }

    this.surface = document.createElement("div");
    this.surface.className = "rich-editor-surface";
    this.surface.contentEditable = "true";
    this.surface.setAttribute("role", "textbox");
    this.surface.setAttribute("aria-multiline", "true");
    this.surface.setAttribute("aria-label", opts.label ?? "Message body");
    this.surface.dataset.placeholder = opts.placeholder ?? "";
    this.surface.innerHTML = opts.value ? bodyToDisplayHtml(opts.value) : "";
    this.surface.addEventListener("input", () => this.onChange?.());
    this.surface.addEventListener("paste", (e) => this.handlePaste(e));
    this.surface.addEventListener("keydown", (e) => this.handleKeydown(e));

    this.element.append(toolbar, this.surface);
    document.addEventListener("selectionchange", this.onSelectionChange);
  }

  /** The current body, sanitized and ready to send or store. */
  getHtml(): string {
    return sanitizeHtml(this.surface.innerHTML);
  }

  /** The current body as plain text, for the client-side "is it empty?" check. */
  getText(): string {
    return htmlToText(this.surface.innerHTML);
  }

  isEmpty(): boolean {
    return isBlankHtml(this.surface.innerHTML);
  }

  setHtml(html: string): void {
    this.surface.innerHTML = html ? bodyToDisplayHtml(html) : "";
  }

  focus(): void {
    this.surface.focus();
  }

  destroy(): void {
    document.removeEventListener("selectionchange", this.onSelectionChange);
  }

  private buildButton(button: ToolbarButton): HTMLButtonElement {
    const el = document.createElement("button");
    el.type = "button";
    el.className = "rich-editor-btn pressable";
    el.title = button.title;
    el.setAttribute("aria-label", button.title);
    el.setAttribute("aria-pressed", "false");
    el.appendChild(icon(button.iconName));
    // Keep the selection in the surface; a normal click would move focus to the
    // button and the command would apply to nothing.
    el.addEventListener("mousedown", (e) => e.preventDefault());
    el.onclick = () => this.run(button);
    this.buttons.set(button.name, el);
    return el;
  }

  private run(button: ToolbarButton): void {
    this.surface.focus();
    if (button.name === "link") {
      const url = window.prompt("Link URL (https:// or mailto:)");
      if (!url) return;
      const trimmed = url.trim();
      if (!/^(https?:\/\/|mailto:)/i.test(trimmed)) {
        this.onChange?.();
        return;
      }
      document.execCommand("createLink", false, trimmed);
    } else {
      document.execCommand(button.command, false, button.value);
    }
    this.onChange?.();
    this.refreshActive();
  }

  private handlePaste(e: ClipboardEvent): void {
    const clip = e.clipboardData;
    if (!clip) return;
    e.preventDefault();
    const html = clip.getData("text/html");
    if (html) {
      document.execCommand("insertHTML", false, sanitizeHtml(html));
    } else {
      document.execCommand("insertText", false, clip.getData("text/plain"));
    }
    this.onChange?.();
  }

  private handleKeydown(e: KeyboardEvent): void {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const shortcuts: Record<string, string> = {
      b: "bold",
      i: "italic",
      u: "underline",
    };
    const command = shortcuts[e.key.toLowerCase()];
    if (!command) return;
    e.preventDefault();
    document.execCommand(command);
    this.onChange?.();
    this.refreshActive();
  }

  /** Mirror the caret's formatting into the toolbar's pressed state. */
  private refreshActive(): void {
    if (!this.surface.contains(document.activeElement)) return;
    const block = String(document.queryCommandValue("formatBlock") || "")
      .replace(/[<>]/g, "")
      .toLowerCase();
    const active: Record<string, boolean> = {
      bold: document.queryCommandState("bold"),
      italic: document.queryCommandState("italic"),
      underline: document.queryCommandState("underline"),
      strike: document.queryCommandState("strikeThrough"),
      ul: document.queryCommandState("insertUnorderedList"),
      ol: document.queryCommandState("insertOrderedList"),
      h2: block === "h2",
      quote: block === "blockquote",
      code: block === "pre",
    };
    for (const [name, el] of this.buttons) {
      const on = active[name] ?? false;
      el.classList.toggle("active", on);
      el.setAttribute("aria-pressed", on ? "true" : "false");
    }
  }
}
