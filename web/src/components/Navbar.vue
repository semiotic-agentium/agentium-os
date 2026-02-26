<script setup lang="ts">
type View = "dashboard" | "chat";

defineProps<{
  view: View;
  agentCount: number;
  theme: string;
}>();

const emit = defineEmits<{
  changeView: [view: View];
  toggleTheme: [];
}>();
</script>

<template>
  <nav class="navbar">
    <!-- Brand -->
    <div class="navbar-brand">
      <!-- Terminal icon -->
      <svg class="brand-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <polyline points="4 17 10 11 4 5" />
        <line x1="12" y1="19" x2="20" y2="19" />
      </svg>
      <span class="brand-name">Agentium</span>
    </div>

    <!-- View tabs -->
    <div class="navbar-tabs">
      <button :class="['nav-tab', { active: view === 'dashboard' }]" @click="emit('changeView', 'dashboard')">
        <!-- Grid icon -->
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <rect x="3" y="3" width="7" height="7" /><rect x="14" y="3" width="7" height="7" />
          <rect x="14" y="14" width="7" height="7" /><rect x="3" y="14" width="7" height="7" />
        </svg>
        Dashboard
      </button>
      <button :class="['nav-tab', { active: view === 'chat' }]" @click="emit('changeView', 'chat')">
        <!-- Chat icon -->
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        </svg>
        Chat Interface
      </button>
    </div>

    <!-- Right: status pills + theme toggle -->
    <div class="navbar-right">
      <span class="status-pill">
        <span class="status-dot" />
        {{ agentCount }} Active Agent{{ agentCount !== 1 ? "s" : "" }}
      </span>
      <span class="status-pill">
        <span class="status-dot" />
        System: Online
      </span>
      <button class="theme-toggle" @click="emit('toggleTheme')" :title="theme === 'light' ? 'Switch to dark mode' : 'Switch to light mode'">
        <!-- Sun icon (shown in dark mode) -->
        <svg v-if="theme === 'dark'" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="12" cy="12" r="5" />
          <line x1="12" y1="1" x2="12" y2="3" /><line x1="12" y1="21" x2="12" y2="23" />
          <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" /><line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
          <line x1="1" y1="12" x2="3" y2="12" /><line x1="21" y1="12" x2="23" y2="12" />
          <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" /><line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
        </svg>
        <!-- Moon icon (shown in light mode) -->
        <svg v-else xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
        </svg>
      </button>
    </div>
  </nav>
</template>
