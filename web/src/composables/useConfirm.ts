import { ref } from "vue";

export interface ConfirmState {
  visible: boolean;
  title: string;
  message: string;
  resolve: ((confirmed: boolean) => void) | null;
}

const state = ref<ConfirmState>({
  visible: false,
  title: "",
  message: "",
  resolve: null,
});

export function useConfirm() {
  function confirm(title: string, message: string): Promise<boolean> {
    return new Promise((resolve) => {
      state.value = { visible: true, title, message, resolve };
    });
  }

  function handleResponse(confirmed: boolean) {
    const { resolve } = state.value;
    state.value = { visible: false, title: "", message: "", resolve: null };
    resolve?.(confirmed);
  }

  return { state, confirm, handleResponse };
}
