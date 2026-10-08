/**
 * A keyboard-typeable contact picker for the mail composer.
 *
 * It replaces a native `<select>` whose options were only reachable with a
 * pointer or by scrolling a long list. This is an editable combobox with a
 * listbox popup (WAI-ARIA 1.2): the text field is the query, the popup is the
 * narrowed list, and the highlighted option is tracked with
 * `aria-activedescendant` so a screen reader follows the arrow keys.
 *
 * It is deliberately not a general autocomplete. One control needs it and the
 * Rule of Three has not been met, so it stays a single-purpose module with a
 * small interface: contacts in, selected nickname out. The caller owns the
 * selection (the composer keeps it in `mailComposeTo` so a rebuild cannot lose
 * it); this control only mirrors that value and reports changes.
 */

export interface ContactPickerItem {
  /** Written back on selection: the contact's nickname. */
  nickname: string;
  /** What the user reads, e.g. "Bob (bob@myserver.com)". */
  label: string;
  /** Extra text the filter matches, on top of the label (e.g. the raw nickname). */
  keywords?: string;
}

export interface ContactPickerOptions {
  items: ContactPickerItem[];
  /** The currently selected nickname, or "" for none. */
  value: string;
  /** Accessible name for the field. */
  label?: string;
  placeholder?: string;
  /**
   * When true the recipient cannot be changed. Used for a reply, where editing
   * the recipient would fork the thread.
   */
  locked?: boolean;
  /** Shown when there are no contacts to choose from at all. */
  emptyMessage?: string;
  /** Shown when the typed query matches no contact. */
  noMatchMessage?: string;
  /** Fired only when the user commits a selection. */
  onChange: (nickname: string) => void;
}

let pickerUid = 0;

/**
 * Case- and accent-insensitive fold, so "alvaro" finds "Álvaro" and "bob@"
 * finds "bob@myserver.com". `NFD` splits a letter from its combining mark and
 * `\p{M}` drops the mark; `toLowerCase` handles case. This is the substring
 * analogue of the `localeCompare(..., { sensitivity: "base" })` used to order
 * Trusts.
 */
function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/** A key that inserts text into the field (letters, digits, space, punctuation). */
function isPrintableKey(e: KeyboardEvent): boolean {
  return e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey;
}

export class ContactPicker {
  readonly element: HTMLElement;
  private readonly input: HTMLInputElement;
  private readonly popup: HTMLElement;
  private readonly list: HTMLElement;
  private readonly empty: HTMLElement;
  private readonly items: ContactPickerItem[];
  private readonly listId: string;
  private readonly locked: boolean;
  private readonly emptyMessage: string;
  private readonly noMatchMessage: string;
  private readonly onChange: (nickname: string) => void;

  /** The committed nickname. The source of truth lives in the caller. */
  private selected: string;
  private open = false;
  /** True while the field holds a query rather than the selected label. */
  private editing = false;
  private query = "";
  private filtered: ContactPickerItem[] = [];
  private highlight = -1;

  constructor(opts: ContactPickerOptions) {
    this.items = opts.items;
    this.selected = opts.value;
    this.locked = opts.locked ?? false;
    this.emptyMessage = opts.emptyMessage ?? "No contacts yet.";
    this.noMatchMessage = opts.noMatchMessage ?? "No contacts match that name.";
    this.onChange = opts.onChange;
    this.listId = `contact-picker-list-${++pickerUid}`;

    this.element = document.createElement("div");
    this.element.className = "contact-picker";

    this.input = document.createElement("input");
    this.input.type = "text";
    this.input.className = "input-field contact-picker-input";
    this.input.placeholder = opts.placeholder ?? "Type a name…";
    this.input.autocomplete = "off";
    this.input.spellcheck = false;
    this.input.setAttribute("role", "combobox");
    this.input.setAttribute("aria-autocomplete", "list");
    this.input.setAttribute("aria-haspopup", "listbox");
    this.input.setAttribute("aria-expanded", "false");
    this.input.setAttribute("aria-controls", this.listId);
    this.input.setAttribute("aria-label", opts.label ?? "Recipient");

    if (this.locked) {
      // Read-only rather than disabled: the recipient stays readable and
      // focusable for a screen reader, but cannot be edited.
      this.input.readOnly = true;
      this.input.setAttribute("aria-readonly", "true");
      this.input.classList.add("contact-picker-locked");
      this.input.title = "A reply goes back to the contact who wrote — the recipient cannot be changed.";
    }

    this.popup = document.createElement("div");
    this.popup.className = "contact-picker-popup";
    this.popup.hidden = true;

    this.list = document.createElement("div");
    this.list.className = "contact-picker-list";
    this.list.id = this.listId;
    this.list.setAttribute("role", "listbox");
    this.list.setAttribute("aria-label", opts.label ?? "Contacts");

    this.empty = document.createElement("p");
    this.empty.className = "contact-picker-empty text-13 muted";
    // Announced, so the empty state is not silent for a screen reader. Plain
    // `role="status"` rather than `aria-live`, because the app's stylesheet
    // moves any `[aria-live]` element off-screen.
    this.empty.setAttribute("role", "status");
    this.empty.hidden = true;

    this.popup.append(this.list, this.empty);
    this.element.append(this.input, this.popup);

    if (!this.locked) {
      this.input.addEventListener("focus", () => this.handleFocus());
      this.input.addEventListener("keydown", (e) => this.handleKeydown(e));
      this.input.addEventListener("input", () => this.handleInput());
      this.input.addEventListener("blur", () => this.close());
    }

    this.renderValue();
    this.refresh();
  }

  /** The committed nickname, or "" when nothing is selected. */
  get value(): string {
    return this.selected;
  }

  /** The label shown for a nickname, falling back to the nickname itself. */
  private labelFor(nickname: string): string {
    return this.items.find((it) => it.nickname === nickname)?.label ?? nickname;
  }

  private handleFocus(): void {
    if (this.locked || this.open) return;
    this.open = true;
    this.editing = false;
    this.query = "";
    this.renderValue();
    this.refresh();
  }

  private handleInput(): void {
    // Covers paste, IME composition, and any edit that did not go through the
    // printable-key path below.
    this.editing = true;
    this.open = true;
    this.query = this.input.value;
    this.refresh();
  }

  private handleKeydown(e: KeyboardEvent): void {
    if (this.locked) return;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        this.openList();
        this.move(1);
        return;
      case "ArrowUp":
        e.preventDefault();
        this.openList();
        this.move(-1);
        return;
      case "Enter":
        // Never let Enter reach the composer: it only commits a highlighted
        // option, and does nothing (rather than falling back to a contact)
        // when there is no highlight.
        e.preventDefault();
        this.commitHighlight();
        return;
      case "Escape":
        if (this.open || this.editing) {
          // Claim the key: the app's document-level handler re-renders the
          // whole shell on Escape, which would rebuild this composer and drop
          // the user's focus. Escape here only closes the list.
          e.preventDefault();
          e.stopPropagation();
          this.close();
        }
        return;
      case "Tab":
        // Let focus move on; blur closes the popup and restores the label.
        this.close();
        return;
    }
    if (!this.editing && (e.key === "Backspace" || e.key === "Delete")) {
      // Editing the label text itself would be a lie about what is selected, so
      // the first edit starts a fresh, empty query instead.
      e.preventDefault();
      this.beginQuery("");
      return;
    }
    if (!this.editing && isPrintableKey(e)) {
      // Replaces the selected label with the first typed character, so typing
      // always narrows from scratch.
      e.preventDefault();
      this.beginQuery(e.key);
    }
  }

  /** Open the popup and show the full list, without moving the highlight. */
  private openList(): void {
    if (this.open) return;
    this.open = true;
    this.editing = false;
    this.query = "";
    this.renderValue();
    this.refresh();
  }

  private beginQuery(text: string): void {
    this.editing = true;
    this.open = true;
    this.query = text;
    this.renderValue();
    this.input.setSelectionRange(text.length, text.length);
    this.refresh();
  }

  private move(delta: number): void {
    if (!this.filtered.length) {
      this.highlight = -1;
      this.markHighlight();
      this.syncAria();
      return;
    }
    if (this.highlight < 0) {
      this.highlight = delta > 0 ? 0 : this.filtered.length - 1;
    } else {
      this.highlight =
        (this.highlight + delta + this.filtered.length) % this.filtered.length;
    }
    this.markHighlight();
    this.syncAria();
  }

  private commitHighlight(): void {
    if (!this.open || this.highlight < 0) return;
    this.select(this.highlight);
  }

  private select(index: number): void {
    const item = this.filtered[index];
    if (!item) return;
    this.selected = item.nickname;
    this.editing = false;
    this.query = "";
    this.open = false;
    this.highlight = -1;
    this.renderValue();
    this.refresh();
    this.onChange(item.nickname);
  }

  private close(): void {
    if (!this.open && !this.editing) return;
    this.open = false;
    this.editing = false;
    this.query = "";
    this.highlight = -1;
    this.renderValue();
    this.refresh();
  }

  /** Recompute the visible list, then repaint it and the ARIA state. */
  private refresh(): void {
    this.filtered = this.computeFiltered();
    this.highlight = this.defaultHighlight();
    this.renderOptions();
    this.popup.hidden = !this.open;
    this.syncAria();
  }

  private computeFiltered(): ContactPickerItem[] {
    const q = fold(this.query.trim());
    if (!q) return this.items;
    return this.items.filter((it) =>
      fold(`${it.label} ${it.keywords ?? ""}`).includes(q),
    );
  }

  /**
   * The starting highlight. A typed query highlights its top match so Enter
   * picks it. An untyped open only highlights the already-selected contact —
   * never the first one, so nothing is chosen without the user acting.
   */
  private defaultHighlight(): number {
    if (!this.open) return -1;
    if (this.query.trim()) return this.filtered.length ? 0 : -1;
    return this.filtered.findIndex((it) => it.nickname === this.selected);
  }

  private renderOptions(): void {
    this.list.replaceChildren();
    this.filtered.forEach((it, i) => {
      const opt = document.createElement("div");
      opt.className = "contact-picker-option";
      opt.id = `${this.listId}-opt-${i}`;
      opt.setAttribute("role", "option");
      opt.setAttribute("aria-selected", String(it.nickname === this.selected));
      if (i === this.highlight) opt.classList.add("is-highlighted");
      opt.textContent = it.label;
      // Keep focus in the input so blur does not close before the click lands.
      opt.addEventListener("mousedown", (e) => e.preventDefault());
      opt.addEventListener("click", () => this.select(i));
      this.list.appendChild(opt);
    });

    const hasOptions = this.filtered.length > 0;
    this.list.hidden = !hasOptions;
    this.empty.hidden = hasOptions;
    if (!hasOptions) {
      // "No contacts at all" and "nothing matched" are different problems.
      this.empty.textContent =
        this.items.length === 0 ? this.emptyMessage : this.noMatchMessage;
    }
  }

  private markHighlight(): void {
    const options = Array.from(this.list.children) as HTMLElement[];
    options.forEach((el, i) =>
      el.classList.toggle("is-highlighted", i === this.highlight),
    );
    options[this.highlight]?.scrollIntoView({ block: "nearest" });
  }

  private syncAria(): void {
    this.input.setAttribute("aria-expanded", String(this.open));
    if (this.open && this.highlight >= 0) {
      this.input.setAttribute(
        "aria-activedescendant",
        `${this.listId}-opt-${this.highlight}`,
      );
    } else {
      this.input.removeAttribute("aria-activedescendant");
    }
  }

  /** Mirror the committed state into the field without disturbing the caret. */
  private renderValue(): void {
    const next = this.editing ? this.query : this.labelFor(this.selected);
    if (this.input.value !== next) this.input.value = next;
  }
}
