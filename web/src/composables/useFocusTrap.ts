import { onUnmounted, watch, type Ref } from "vue";

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function useFocusTrap(containerRef: Ref<HTMLElement | null>, active: Ref<boolean>) {
  let previousFocus: HTMLElement | null = null;

  function trapFocus(e: KeyboardEvent) {
    if (e.key !== "Tab") return;
    const container = containerRef.value;
    if (!container) return;

    const focusable = Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
    if (focusable.length === 0) return;

    const first = focusable[0]!;
    const last = focusable[focusable.length - 1]!;

    if (e.shiftKey) {
      if (document.activeElement === first) {
        e.preventDefault();
        last.focus();
      }
    } else {
      if (document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  watch(active, (isActive) => {
    if (isActive) {
      previousFocus = document.activeElement as HTMLElement;
      requestAnimationFrame(() => {
        const container = containerRef.value;
        if (!container) return;
        const first = container.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
        first?.focus();
      });
      document.addEventListener("keydown", trapFocus);
    } else {
      document.removeEventListener("keydown", trapFocus);
      previousFocus?.focus();
      previousFocus = null;
    }
  });

  onUnmounted(() => {
    document.removeEventListener("keydown", trapFocus);
  });
}
