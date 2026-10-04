// In-app updates: the one-time consent question, the "update available" banner with download
// progress, and the release-notes dialog. Network access and signature checks happen in the
// backend; this module only talks to `UpdateApi`.

import type { Settings, UpdateApi, UpdateInfo } from "./api";
import { modal } from "./dialogs";
import { clear, h, icon } from "./dom";
import { bytes } from "./format";
import { I } from "./icons";

export interface UpdateHooks {
  settings(): Settings;
  saveSettings(s: Settings): Promise<void>;
  toast(message: string, error?: boolean): void;
}

export class Updates {
  readonly el: HTMLElement;
  private info: UpdateInfo | null = null;
  private installing = false;

  constructor(private api: UpdateApi, private hooks: UpdateHooks) {
    this.el = h("div", { class: "banner update", hidden: true, role: "status", "aria-live": "polite" });
    api.onAvailable((i) => this.show(i));
    api.onProgress((done, total) => this.progress(done, total));
    api.onRestarting((e) => this.message(`The update could not be installed (${e}). FastFind is restarting…`));
  }

  /** Ask once whether automatic checks are allowed; until answered, nothing is contacted. */
  start(): void {
    if (this.hooks.settings().updates.checkAutomatically === null) this.askConsent();
  }

  private askConsent(): void {
    const answer = async (allow: boolean) => {
      const s = structuredClone(this.hooks.settings());
      s.updates.checkAutomatically = allow;
      try {
        await this.hooks.saveSettings(s);
      } catch (e) {
        this.hooks.toast(String(e), true);
        return;
      }
      this.hide();
      if (allow) void this.checkNow(false);
      else this.hooks.toast("You can check for updates any time in Settings → Updates");
    };
    this.render(
      icon(I.refresh),
      h(
        "span",
        { class: "update-text" },
        h("strong", {}, "Check for updates automatically?"),
        " Once a day FastFind asks GitHub whether a newer version exists. Nothing about your files is sent.",
      ),
      h("button", { class: "btn primary", onclick: () => void answer(true) }, "Check automatically"),
      h("button", { class: "btn", onclick: () => void answer(false) }, "Don't check"),
    );
  }

  /** Manual or first check. `report` shows "up to date" and errors; the background check is silent. */
  async checkNow(report = true): Promise<UpdateInfo | null> {
    try {
      const info = await this.api.check();
      if (info) this.show(info);
      else if (report) this.hooks.toast("FastFind is up to date");
      return info;
    } catch (e) {
      if (report) this.hooks.toast(String(e), true);
      return null;
    }
  }

  show(info: UpdateInfo): void {
    if (this.installing) return;
    this.info = info;
    const action = info.canInstall
      ? h("button", { class: "btn primary", onclick: () => void this.install() }, "Install and restart")
      : h("button", { class: "btn primary", onclick: () => void this.api.openReleasePage() }, "Download");
    this.render(
      icon(I.refresh),
      h("span", { class: "update-text" }, h("strong", {}, `FastFind ${info.version} is available.`), ` You have ${info.currentVersion}.`),
      info.notes ? h("button", { class: "btn ghost", onclick: () => this.showNotes() }, "What's new") : null,
      action,
      h("button", { class: "btn ghost", onclick: () => this.hide() }, "Later"),
    );
  }

  private showNotes(): void {
    const info = this.info;
    if (!info?.notes) return;
    // Plain text: release notes come from the network and are never rendered as HTML.
    const body = h("div", { class: "release-notes" });
    body.textContent = info.notes;
    modal(`What's new in FastFind ${info.version}`, body, [], "wide");
  }

  private async install(): Promise<void> {
    if (!this.info) return;
    this.installing = true;
    this.message(`Downloading FastFind ${this.info.version}…`);
    try {
      // Resolves only if installing failed; on success the app restarts.
      await this.api.install();
    } catch (e) {
      this.installing = false;
      this.hooks.toast(String(e), true);
      this.show(this.info);
    }
  }

  private progress(done: number, total: number | null): void {
    if (!this.installing || !this.info) return;
    const pct = total ? Math.min(100, Math.round((done / total) * 100)) : null;
    if (total !== null && done >= total) {
      this.message(`Installing FastFind ${this.info.version}… FastFind will restart.`);
      return;
    }
    this.message(
      pct === null ? `Downloading FastFind ${this.info.version}… ${bytes(done)}` : `Downloading FastFind ${this.info.version}… ${pct}% (${bytes(done)} of ${bytes(total!)})`,
      pct,
    );
  }

  private message(text: string, pct: number | null = null): void {
    this.render(
      h(
        "div",
        { class: "update-progress" },
        h("span", {}, text),
        h("div", { class: "progress" }, h("div", { class: `bar${pct === null ? " indeterminate" : ""}`, style: pct === null ? "" : `width:${pct}%` })),
      ),
    );
  }

  private render(...children: (Node | null)[]): void {
    clear(this.el);
    for (const c of children) if (c) this.el.appendChild(c);
    this.el.hidden = false;
  }

  private hide(): void {
    this.el.hidden = true;
    clear(this.el);
  }
}
