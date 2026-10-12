// Global shortcuts in Tauri's accelerator syntax ("CommandOrControl+Shift+F"): recording one
// from a key press, and showing one the way the operating system writes it.

import { isMac } from "./dom";

export const DEFAULT_SHORTCUT = "CommandOrControl+Shift+F";

const MODIFIER_CODES = new Set(["ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "OSLeft", "OSRight"]);

/**
 * The accelerator for a key press, or null while only modifiers are held or when no Ctrl,
 * Alt or ⌘ is involved (a global shortcut without one would swallow ordinary typing).
 */
export function acceleratorFromEvent(e: KeyboardEvent, mac = isMac): string | null {
  if (MODIFIER_CODES.has(e.code) || !e.code) return null;
  const mods: string[] = [];
  if (mac) {
    if (e.metaKey) mods.push("CommandOrControl");
    if (e.ctrlKey) mods.push("Control");
  } else {
    if (e.ctrlKey) mods.push("CommandOrControl");
    if (e.metaKey) mods.push("Super");
  }
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (!(e.ctrlKey || e.metaKey || e.altKey)) return null;
  return [...mods, e.code].join("+");
}

/**
 * The key press an accelerator stands for. A global shortcut never reaches FastFind's own
 * window, so when it is pressed there the app replays it as an ordinary key press.
 */
export function keyEventFromAccelerator(acc: string, mac = isMac): KeyboardEvent {
  const init: KeyboardEventInit = { bubbles: true, cancelable: true };
  let key = "";
  for (const raw of acc.split("+")) {
    const p = raw.trim();
    switch (p.toLowerCase()) {
      case "commandorcontrol": case "commandorctrl": case "cmdorctrl": case "cmdorcontrol":
        if (mac) init.metaKey = true;
        else init.ctrlKey = true;
        break;
      case "control": case "ctrl": init.ctrlKey = true; break;
      case "super": case "command": case "cmd": init.metaKey = true; break;
      case "alt": case "option": init.altKey = true; break;
      case "shift": init.shiftKey = true; break;
      default: key = p;
    }
  }
  const bare = key.replace(/^(Key|Digit)/i, "");
  init.code = /^[a-z]$/i.test(bare) ? `Key${bare.toUpperCase()}` : /^\d$/.test(bare) ? `Digit${bare}` : key;
  init.key = bare.length === 1 ? (init.shiftKey ? bare.toUpperCase() : bare.toLowerCase()) : bare;
  return new KeyboardEvent("keydown", init);
}

/** "CommandOrControl+Shift+KeyF" → "Ctrl+Shift+F" (Windows, Linux) or "⌘⇧F" (macOS). */
export function displayShortcut(acc: string, mac = isMac): string {
  const parts = acc.split("+").map((raw) => {
    const p = raw.trim().toLowerCase();
    switch (p) {
      case "commandorcontrol": case "commandorctrl": case "cmdorctrl": case "cmdorcontrol": return mac ? "⌘" : "Ctrl";
      case "control": case "ctrl": return mac ? "⌃" : "Ctrl";
      case "alt": case "option": return mac ? "⌥" : "Alt";
      case "shift": return mac ? "⇧" : "Shift";
      case "super": case "command": case "cmd": return mac ? "⌘" : "Win";
    }
    const k = raw.trim().replace(/^(Key|Digit)/i, "");
    return k.charAt(0).toUpperCase() + k.slice(1);
  });
  return parts.join(mac ? "" : "+");
}
