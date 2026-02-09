/**
 * A2A runtime shim: register handler and tools on globalThis so the runner can invoke them.
 * Loaded before agent code (prepended to dist/index.js) or evaluated first.
 * Host overwrites __baml_a2a_yield for stream requests; no-op here so the symbol always exists.
 */
(function () {
  globalThis.__baml_a2a_yield = function (chunk) {
    if (globalThis.__baml_a2a_yield_buffer) globalThis.__baml_a2a_yield_buffer.push(chunk);
  };
  function __baml_a2a_register(agent) {
    if (agent.handle_a2a_request != null) {
      globalThis.handle_a2a_request = agent.handle_a2a_request;
    }
    if (agent.handle_a2a_cancel != null) {
      globalThis.handle_a2a_cancel = agent.handle_a2a_cancel;
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
  globalThis.__baml_a2a_register = __baml_a2a_register;
})();
