/**
 * Chat runtime shim: register handler and tools on globalThis so the runner can invoke them.
 * Loaded before agent code (prepended to dist/index.js) or evaluated first.
 * Host overwrites __baml_chat_yield for stream requests; no-op here so the symbol always exists.
 * The JS interface is message-only; IDs and protocol metadata are host-managed.
 */
(function () {
  globalThis.__baml_chat_yield = function (chunk) {
    if (globalThis.__baml_chat_yield_buffer) globalThis.__baml_chat_yield_buffer.push(chunk);
  };
  function __baml_chat_register(agent) {
    if (agent.onChatMessage != null) {
      globalThis.onChatMessage = agent.onChatMessage;
    }
    if (agent.tools != null && typeof agent.tools === 'object') {
      globalThis.__js_tools = globalThis.__js_tools || {};
      for (var name in agent.tools) {
        if (Object.prototype.hasOwnProperty.call(agent.tools, name)) {
          globalThis.__js_tools[name] = agent.tools[name];
        }
      }
    }
  }
  globalThis.__baml_chat_register = __baml_chat_register;
})();
