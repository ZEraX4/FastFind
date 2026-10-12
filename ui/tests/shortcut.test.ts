import { describe, expect, it } from "vitest";
import { acceleratorFromEvent, displayShortcut, keyEventFromAccelerator } from "../src/shortcut";

const key = (code: string, mods: Partial<KeyboardEventInit> = {}) => new KeyboardEvent("keydown", { code, ...mods });

describe("global shortcuts", () => {
  it("records Ctrl/⌘ as CommandOrControl so the setting works on every platform", () => {
    expect(acceleratorFromEvent(key("KeyF", { ctrlKey: true, shiftKey: true }), false)).toBe("CommandOrControl+Shift+KeyF");
    expect(acceleratorFromEvent(key("KeyF", { metaKey: true, shiftKey: true }), true)).toBe("CommandOrControl+Shift+KeyF");
    expect(acceleratorFromEvent(key("Space", { ctrlKey: true, altKey: true }), false)).toBe("CommandOrControl+Alt+Space");
    expect(acceleratorFromEvent(key("Space", { ctrlKey: true }), true)).toBe("Control+Space");
  });

  it("waits for a real key and refuses shortcuts that would swallow typing", () => {
    expect(acceleratorFromEvent(key("ShiftLeft", { shiftKey: true, ctrlKey: true }), false)).toBeNull();
    expect(acceleratorFromEvent(key("KeyA"), false)).toBeNull();
    expect(acceleratorFromEvent(key("KeyA", { shiftKey: true }), false)).toBeNull();
    expect(acceleratorFromEvent(key("F5", { altKey: true }), false)).toBe("Alt+F5");
  });

  it("replays an accelerator as the key press it stands for", () => {
    const e = keyEventFromAccelerator("CommandOrControl+Shift+F", false);
    expect([e.key, e.code, e.ctrlKey, e.shiftKey, e.metaKey, e.altKey]).toEqual(["F", "KeyF", true, true, false, false]);
    const m = keyEventFromAccelerator("CommandOrControl+Alt+KeyK", true);
    expect([m.key, m.code, m.metaKey, m.altKey, m.ctrlKey]).toEqual(["k", "KeyK", true, true, false]);
    expect(keyEventFromAccelerator("Control+Space", true).code).toBe("Space");
  });

  it("shows shortcuts the way the system writes them", () => {
    expect(displayShortcut("CommandOrControl+Shift+F", false)).toBe("Ctrl+Shift+F");
    expect(displayShortcut("CommandOrControl+Shift+KeyF", false)).toBe("Ctrl+Shift+F");
    expect(displayShortcut("CommandOrControl+Shift+KeyF", true)).toBe("⌘⇧F");
    expect(displayShortcut("Control+Alt+Digit1", true)).toBe("⌃⌥1");
    expect(displayShortcut("Super+Space", false)).toBe("Win+Space");
  });
});
