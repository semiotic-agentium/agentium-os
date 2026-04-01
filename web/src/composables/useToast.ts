import { ref } from "vue";

export interface Toast {
  id: number;
  message: string;
  type: "success" | "error" | "info";
  leaving?: boolean;
}

const toasts = ref<Toast[]>([]);
let nextId = 0;

function show(message: string, type: Toast["type"] = "info", durationMs = 3500): void {
  const id = nextId++;
  toasts.value.push({ id, message, type });
  setTimeout(() => dismiss(id), durationMs);
}

function dismiss(id: number): void {
  const toast = toasts.value.find((t) => t.id === id);
  if (!toast) return;
  toast.leaving = true;
  setTimeout(() => {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }, 220);
}

export function useToast() {
  return {
    toasts,
    show,
    dismiss,
    success: (msg: string) => show(msg, "success"),
    error: (msg: string) => show(msg, "error"),
  };
}
