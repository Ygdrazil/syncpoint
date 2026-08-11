import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Theme } from "./types";

/// Which native appearance each theme's title bar and scrollbars should follow.
/// Native chrome only has light and dark, so every theme has to declare one —
/// Catppuccin Latte is light while its three siblings are dark, so this cannot
/// be inferred from the name.
const NATIVE: Record<Theme, "light" | "dark" | null> = {
  system: null,
  light: "light",
  dark: "dark",
  "catppuccin-latte": "light",
  "catppuccin-frappe": "dark",
  "catppuccin-macchiato": "dark",
  "catppuccin-mocha": "dark"
};

/// Apply a theme to the current window.
///
/// Removing `data-theme` rather than setting it to "system" matters: the CSS
/// treats the attribute's absence as "follow `prefers-color-scheme`", so any
/// explicit theme wins over the OS preference.
export async function applyTheme(theme: Theme): Promise<void> {
  const root = document.documentElement;
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }

  try {
    await getCurrentWindow().setTheme(NATIVE[theme] ?? null);
  } catch {
    // Not fatal: the CSS above already themed everything inside the webview.
  }
}
