import type { ComputedRef, InjectionKey, Ref } from "vue";
import { computed, ref, watch } from "vue";
import { buildPlanStepDescriptionLookup } from "../chat/planStepLookup";
import type { ContextPlanningResponse, ContextPlanningTaskSnapshot } from "../types/provenance";

export type ContextPlanningApi = {
  tasks: ComputedRef<ContextPlanningTaskSnapshot[]>;
  allTaskIds: ComputedRef<string[]>;
  loading: Readonly<Ref<boolean>>;
  error: Readonly<Ref<string | null>>;
  refresh: () => Promise<void>;
  planStepDescriptionLookup: ComputedRef<Map<string, string>>;
};

export const CONTEXT_PLANNING_INJECTION_KEY: InjectionKey<ContextPlanningApi> =
  Symbol.for("baml.contextPlanning");

export function useContextPlanning(
  contextId: Ref<string | undefined> | ComputedRef<string | undefined>,
): ContextPlanningApi {
  const response = ref<ContextPlanningResponse | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function refresh(): Promise<void> {
    const id = contextId.value?.trim();
    if (!id) {
      response.value = null;
      error.value = null;
      return;
    }
    loading.value = true;
    error.value = null;
    try {
      const res = await fetch(`/contexts/${id}/planning`);
      if (!res.ok) {
        if (res.status === 404) {
          response.value = null;
          return;
        }
        throw new Error(`Planning request failed: ${res.status}`);
      }
      response.value = (await res.json()) as ContextPlanningResponse;
    } catch (e) {
      error.value = (e as Error).message;
    } finally {
      loading.value = false;
    }
  }

  watch(
    contextId,
    () => {
      void refresh();
    },
    { immediate: true },
  );

  const tasks = computed(() => response.value?.tasks ?? []);
  const allTaskIds = computed(() => response.value?.allTaskIds ?? []);
  const planStepDescriptionLookup = computed(() => buildPlanStepDescriptionLookup(tasks.value));

  return {
    tasks,
    allTaskIds,
    loading,
    error,
    refresh,
    planStepDescriptionLookup,
  };
}
