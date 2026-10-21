/**
 * A three-dots overflow menu: a small anchored popover of actions.
 *
 * It is the popup shape `ContactPicker` uses — anchored, overlaid rather than
 * pushing content, and driven by the keyboard — but for a short list of
 * commands instead of a listbox. The trigger owns its own open state and its
 * own DOM, so a caller can drop it into a header without threading an "is open"
 * flag through the app's render loop.
 *
 * Opening it never re-renders the app. A caller's `onSelect` does the
 * re-render when it needs one, which is also how the menu gets rebuilt with a
 * fresh checked state.
 */
import { icon, type IconName } from "../../lib/icons";

export interface OverflowMenuItem {
  /** Stable id, so a caller and its tests can name a row. */
  id: string;
  label: string;
  icon?: IconName;
  /** Renders in the danger colour. Put it last, after a separator. */
  danger?: boolean;
  /**
   * A toggle row. `true`/`false` renders a check when on; leave undefined for a
   * plain command.
   */
  checked?: boolean;
  /** Draw a rule above this row, to group the rows that follow. */
  separatorBefore?: boolean;
  /**
   * Show a chevron, marking the row as one that leaves this surface (opens a
   * dialog or another rail) rather than acting in place.
   */
  chevron?: boolean;
  onSelect: () => void;
}

export interface OverflowMenuOptions {
  items: OverflowMenuItem[];
  /** Accessible name for the trigger, e.g. "Options for Ada". */
  label: string;
  /** The trigger's icon. Defaults to the three-dot menu. */
  icon?: IconName;
  /** Extra class on the trigger, for a caller that styles its own control. */
  triggerClass?: string;
  /**
   * A container whose presses count as inside the menu: a row whose whole
   * click is the menu. Without it, the close-on-mousedown would fire before
   * the row's own click handler, so a row click that opened the menu could
   * never close it again.
   */
  surface?: HTMLElement;
}

let menuUid = 0;

export class OverflowMenu {
  readonly element: HTMLElement;
  private readonly trigger: HTMLButtonElement;
  private readonly popup: HTMLElement;
  private readonly items: OverflowMenuItem[];
  private readonly menuId: string;
  private readonly surface: HTMLElement | null;
  private open = false;

  /**
   * Close on a press anywhere else. Capture phase, so it lands before the
   * press's own handler; the element's own subtree is exempt, which is what
   * lets the trigger toggle and a row activate.
   */
  private readonly onOutsideDown = (e: MouseEvent): void => {
    if (!this.element.isConnected) {
      // The app re-rendered and dropped this menu out of the document without a
      // close. Unhook now rather than leak the listener.
      document.removeEventListener("mousedown", this.onOutsideDown, true);
      return;
    }
    const scope = this.surface ?? this.element;
    if (!scope.contains(e.target as Node)) this.close();
  };

  constructor(opts: OverflowMenuOptions) {
    this.items = opts.items;
    this.surface = opts.surface ?? null;
    this.menuId = `overflow-menu-${++menuUid}`;

    this.element = document.createElement("div");
    this.element.className = "overflow-menu";

    this.trigger = document.createElement("button");
    this.trigger.type = "button";
    this.trigger.className = "btn-icon-round pressable overflow-trigger";
    if (opts.triggerClass) this.trigger.classList.add(opts.triggerClass);
    this.trigger.setAttribute("aria-label", opts.label);
    this.trigger.title = opts.label;
    this.trigger.setAttribute("aria-haspopup", "menu");
    this.trigger.setAttribute("aria-expanded", "false");
    this.trigger.setAttribute("aria-controls", this.menuId);
    this.trigger.appendChild(icon(opts.icon ?? "more"));
    this.trigger.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggle();
    });

    this.popup = document.createElement("div");
    this.popup.className = "overflow-popup";
    this.popup.id = this.menuId;
    this.popup.setAttribute("role", "menu");
    this.popup.setAttribute("aria-label", opts.label);
    this.popup.hidden = true;
    this.popup.addEventListener("keydown", (e) => this.handleKeydown(e));

    this.element.append(this.trigger, this.popup);
    this.renderItems();
  }

  private renderItems(): void {
    this.popup.replaceChildren();
    for (const it of this.items) {
      if (it.separatorBefore) {
        const rule = document.createElement("div");
        rule.className = "overflow-sep";
        rule.setAttribute("role", "separator");
        this.popup.appendChild(rule);
      }

      const row = document.createElement("button");
      row.type = "button";
      row.className = `overflow-item${it.danger ? " danger" : ""}`;
      row.dataset.id = it.id;
      const checkbox = it.checked !== undefined;
      row.setAttribute("role", checkbox ? "menuitemcheckbox" : "menuitem");
      if (checkbox) row.setAttribute("aria-checked", String(it.checked));

      if (it.icon) row.appendChild(icon(it.icon));

      const label = document.createElement("span");
      label.className = "overflow-item-label";
      label.textContent = it.label;
      row.appendChild(label);

      // A trailing column: a check for a toggle, a chevron for a row that
      // leaves this surface, nothing otherwise. Always present so labels line up.
      const mark = document.createElement("span");
      mark.className = "overflow-item-mark";
      if (checkbox && it.checked) mark.appendChild(icon("check"));
      else if (it.chevron) mark.appendChild(icon("chevronRight"));
      row.appendChild(mark);

      row.addEventListener("click", () => {
        this.close();
        it.onSelect();
      });
      this.popup.appendChild(row);
    }
  }

  private toggle(): void {
    if (this.open) this.close();
    else this.openMenu();
  }

  /**
   * Toggle from outside the trigger, for a `surface` whose whole click is the
   * menu (a row that opens its actions). The surface is exempt from the
   * close-on-mousedown, so the click reaches this toggle and a row press
   * closes what a row press opened.
   */
  toggleMenu(): void {
    this.toggle();
  }

  private openMenu(): void {
    this.open = true;
    this.popup.hidden = false;
    this.trigger.setAttribute("aria-expanded", "true");
    document.addEventListener("mousedown", this.onOutsideDown, true);
    this.rows()[0]?.focus();
  }

  private close(): void {
    if (!this.open) return;
    this.open = false;
    this.popup.hidden = true;
    this.trigger.setAttribute("aria-expanded", "false");
    document.removeEventListener("mousedown", this.onOutsideDown, true);
  }

  private rows(): HTMLElement[] {
    return Array.from(this.popup.querySelectorAll<HTMLElement>(".overflow-item"));
  }

  private handleKeydown(e: KeyboardEvent): void {
    const rows = this.rows();
    const at = rows.indexOf(document.activeElement as HTMLElement);
    switch (e.key) {
      case "Escape":
        // The app re-renders the shell on a document-level Escape. Claim the
        // key so the menu closes without rebuilding everything under the user.
        e.preventDefault();
        e.stopPropagation();
        this.close();
        this.trigger.focus();
        return;
      case "ArrowDown":
        e.preventDefault();
        rows[at < 0 ? 0 : (at + 1) % rows.length]?.focus();
        return;
      case "ArrowUp":
        e.preventDefault();
        rows[at < 0 ? rows.length - 1 : (at - 1 + rows.length) % rows.length]?.focus();
        return;
      case "Tab":
        // Let focus leave; the popup closes behind it.
        this.close();
        return;
    }
  }
}
