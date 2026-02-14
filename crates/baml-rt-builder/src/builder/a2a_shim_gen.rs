//! A2A runtime shim generator (library code).
//!
//! Produces the JS IIFE prepended to agent dist/index.js. Same for every agent;
//! generated at compile time when the compiler runs. Types agents need are
//! bootstrap-generated in baml-runtime.d.ts, not here.

use baml_rt_core::{BamlRtError, Result};
use genco::fmt::Error as GencoFmtError;
use genco::lang::js;
use genco::prelude::*;
use std::fmt;

/// Wrapper so genco fmt errors can be used as [`std::error::Error`] source.
/// genco's fmt::Error does not implement Error; this preserves the chain in BamlRtError.
#[derive(Debug)]
struct GencoRenderError(GencoFmtError);

impl fmt::Display for GencoRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for GencoRenderError {}

/// Generates the A2A runtime shim (IIFE) that provides session(), __chat_yield,
/// and __chat_register on globalThis. Emit and message-text logic are internal to the DSL.
pub fn render_a2a_shim() -> Result<String> {
    let shim_js = r#"
/**
 * A2A runtime shim (library code). Prepended to agent dist/index.js.
 * Public surface: session(message) and __chat_register. Host sets __chat_yield for stream requests.
 * Session lifecycle: run(fn) executes fn(); { message } -> emit message + completed; { error } or throw -> emit failed.
 * Await-input rail: emit.awaitInput(prompt) emits INPUT_REQUIRED, suspends, and resumes on next message with same task/context.
 * Defaults: missing text part yields ""; rejected promise is treated as { error: err.message }.
 */
(function () {
  globalThis.__chat_yield = function (chunk) {
    if (globalThis.__chat_yield_buffer) globalThis.__chat_yield_buffer.push(chunk);
  };

  function newMessage(text) {
    return { parts: [{ text: text }] };
  }

  function emitMessage(text) {
    var msg = newMessage(text);
    globalThis.__chat_yield({ message: msg, task: { status: { state: "TASK_STATE_WORKING", message: msg } } });
  }

  function emitCompleted() {
    globalThis.__chat_yield({ task: { status: { state: "TASK_STATE_COMPLETED" } } });
  }

  function emitFailed(message, retryable) {
    if (retryable === undefined) retryable = false;
    var msg = newMessage(message);
    globalThis.__chat_yield({ task: { status: { state: "TASK_STATE_FAILED", message: msg } } });
  }

  function emitStatusChanged(to) {
    globalThis.__chat_yield({ statusUpdate: { status: { state: to } } });
  }

  function emitInputRequired(prompt) {
    var status = { state: "TASK_STATE_INPUT_REQUIRED" };
    if (typeof prompt === 'string' && prompt.length > 0) {
      status.message = newMessage(prompt);
    }
    globalThis.__chat_yield({ statusUpdate: { status: status } });
  }

  function emitArtifact(artifact, append, lastChunk) {
    globalThis.__chat_yield({ artifactUpdate: { artifact: artifact, append: !!append, lastChunk: !!lastChunk } });
  }

  function sessionKey(message) {
    if (message && typeof message === 'object') {
      if (typeof message.contextId === 'string' && message.contextId.length > 0) return "ctx:" + message.contextId;
      if (typeof message.context_id === 'string' && message.context_id.length > 0) return "ctx:" + message.context_id;
      if (typeof message.taskId === 'string' && message.taskId.length > 0) return "task:" + message.taskId;
      if (typeof message.task_id === 'string' && message.task_id.length > 0) return "task:" + message.task_id;
      if (message.task && typeof message.task.id === 'string' && message.task.id.length > 0) return "task:" + message.task.id;
    }
    return "__default__";
  }

  var pendingInputResolvers = new Map();

  function messageText(message) {
    if (!message || typeof message !== 'object') return '';
    var parts = message.parts;
    if (!Array.isArray(parts) || parts.length === 0) return '';
    var first = parts[0];
    if (first != null && typeof first.text === 'string') return first.text;
    return '';
  }

  /** Returns a message object with a .text() method (first text part). Used for awaitInput result. */
  function messageWithText(message) {
    if (message != null && typeof message === 'object' && typeof message.text === 'function') return message;
    return Object.assign({}, message, { text: function () { return messageText(message); } });
  }

  function session(message) {
    var key = sessionKey(message);
    var routedToPendingInput = false;
    if (pendingInputResolvers.has(key)) {
      var resolvePending = pendingInputResolvers.get(key);
      pendingInputResolvers.delete(key);
      resolvePending(messageWithText(message));
      routedToPendingInput = true;
    }
    var onCompletedCb = null;
    var onFailedCb = null;
    var emittedMessage = false;
    return {
      text: function () { return messageText(message); },
      onCompleted: function (fn) { onCompletedCb = fn; return this; },
      onFailed: function (fn) { onFailedCb = fn; return this; },
      run: function (fn) {
        if (routedToPendingInput) return Promise.resolve();
        var emit = {
          message: function (text) { emittedMessage = true; emitMessage(text); },
          artifact: emitArtifact,
          statusChanged: emitStatusChanged,
          awaitInput: function (prompt) {
            emitInputRequired(prompt);
            return new Promise(function (resolve) {
              pendingInputResolvers.set(key, resolve);
            });
          }
        };
        var work = (typeof fn.length === 'number' && fn.length >= 1) ? function () { return fn(emit); } : fn;
        return Promise.resolve().then(work).then(function (out) {
          if (out == null) {
            if (emittedMessage) {
              emitCompleted();
              return;
            }
          }
          if (out != null && typeof out === 'object' && 'message' in out && typeof out.message === 'string') {
            if (typeof onCompletedCb === 'function') onCompletedCb(out.message);
            emitMessage(out.message);
            emitCompleted();
            return;
          }
          var err = (out != null && typeof out === 'object' && 'error' in out) ? String(out.error) : 'Unknown error';
          if (typeof onFailedCb === 'function') onFailedCb(err);
          emitFailed(err, false);
        }).catch(function (err) {
          var msg = (err && err != null && typeof err.message === 'string') ? err.message : String(err);
          if (typeof onFailedCb === 'function') onFailedCb(msg);
          emitFailed(msg, false);
        });
      }
    };
  }
  globalThis.session = session;
  globalThis.messageText = messageText;

  function __chat_register(agent) {
    if (agent.run != null && typeof agent.run === 'function') {
      globalThis.onChatMessage = async function (message) {
        var s = session(message);
        await s.run(function (emit) {
          return agent.run({ text: s.text() || '', message: message, emit: emit });
        });
      };
    } else if (agent.onChatMessage != null) {
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
  globalThis.__chat_register = __chat_register;

  /**
   * ReAct-style loop helper.
   * plan(ctx) -> { kind: "final", message } | { kind: "tool", tool, args }
   * execute(step) -> observation (any)
   */
  async function runReActLoop(opts) {
    if (!opts || typeof opts.plan !== 'function' || typeof opts.execute !== 'function') {
      throw new Error('runReActLoop requires { plan, execute }');
    }
    var maxSteps = (opts.maxSteps != null) ? opts.maxSteps : 5;
    var observations = Array.isArray(opts.observations) ? opts.observations.slice() : [];
    var seen = new Set();
    for (var i = 0; i < maxSteps; i++) {
      var plan = await opts.plan({ observations: observations.slice(), step: i });
      if (plan && plan.kind === 'final') return plan.message || '';
      if (!plan || plan.kind !== 'tool') {
        throw new Error('runReActLoop expected { kind: \"tool\" | \"final\" }');
      }
      var key = opts.dedupeKey ? String(opts.dedupeKey(plan)) : JSON.stringify(plan);
      if (seen.has(key)) {
        throw new Error('runReActLoop detected repeated tool call');
      }
      seen.add(key);
      if (typeof opts.onStep === 'function') opts.onStep(plan, i);
      var observation = await opts.execute(plan);
      observations.push({ plan: plan, observation: observation });
    }
    throw new Error('runReActLoop exceeded maxSteps');
  }
  globalThis.runReActLoop = runReActLoop;

  async function runReActLoopHost(token, opts) {
    if (!token) {
      throw new Error('Missing invocation token for runReActLoopHost.');
    }
    return await __run_react_loop_host(token, JSON.stringify(opts || {}));
  }
  globalThis.runReActLoopHost = runReActLoopHost;
})();
"#;

    let mut tokens: js::Tokens = quote!();
    for line in shim_js.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            tokens.line();
        } else {
            quote_in!(tokens => $(trimmed));
            tokens.push();
        }
    }
    tokens
        .to_file_string()
        .map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: "A2A shim render error".into(),
            source: Box::new(GencoRenderError(e)),
        })
}
