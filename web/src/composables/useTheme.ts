import { ref, watchEffect } from "vue";

type Theme = "light" | "dark";

const STORAGE_KEY = "agent-chat-theme";

function getSystemTheme(): Theme {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function getStoredTheme(): Theme | null {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" ? stored : null;
}

const theme = ref<Theme>(getStoredTheme() ?? getSystemTheme());

export function useTheme() {
  watchEffect(() => {
    document.documentElement.setAttribute("data-theme", theme.value);
    localStorage.setItem(STORAGE_KEY, theme.value);
  });

  function toggle() {
    theme.value = theme.value === "light" ? "dark" : "light";
  }

  return { theme, toggle };
}
