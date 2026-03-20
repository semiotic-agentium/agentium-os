//! A2A runtime shim generator (library code).
//!
//! Produces the JS IIFE prepended to agent dist/index.js. Same for every agent;
//! generated at compile time when the compiler runs. Types agents need are
//! bootstrap-generated in baml-runtime.d.ts, not here.

use std::fmt;

use genco::{fmt::Error as GencoFmtError, lang::js, prelude::*};

use crate::builder::error::{BamlBuilderError, Result};

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
  var __chat_yield_host = globalThis.__chat_yield;
  globalThis.__chat_yield = function (chunk) {
    if (typeof __chat_yield_host !== "function") {
      throw new Error("__chat_yield host sink is not installed");
    }
    __chat_yield_host(chunk);
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

  function sessionKeys(message) {
    var keys = [];
    if (message && typeof message === 'object') {
      if (typeof message.taskId === 'string' && message.taskId.length > 0) keys.push("task:" + message.taskId);
      if (message.task && typeof message.task.id === 'string' && message.task.id.length > 0) {
        var taskKey = "task:" + message.task.id;
        if (keys.indexOf(taskKey) < 0) keys.push(taskKey);
      }
      if (typeof message.contextId === 'string' && message.contextId.length > 0) keys.push("ctx:" + message.contextId);
    }
    if (keys.length === 0) keys.push("__default__");
    return keys;
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
    var keys = sessionKeys(message);
    var routedToPendingInput = false;
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      if (pendingInputResolvers.has(key)) {
        var resolvePending = pendingInputResolvers.get(key);
        // Delete all aliases for this session so future turns do not double-resolve.
        for (var j = 0; j < keys.length; j++) {
          pendingInputResolvers.delete(keys[j]);
        }
        resolvePending(messageWithText(message));
        routedToPendingInput = true;
        break;
      }
    }
    var onCompletedCb = null;
    var onFailedCb = null;
    return {
      text: function () { return messageText(message); },
      onCompleted: function (fn) { onCompletedCb = fn; return this; },
      onFailed: function (fn) { onFailedCb = fn; return this; },
      run: function (fn) {
        if (routedToPendingInput) return Promise.resolve();
        var emit = {
          message: emitMessage,
          artifact: emitArtifact,
          statusChanged: emitStatusChanged,
          awaitInput: function (prompt) {
            emitInputRequired(prompt);
            return new Promise(function (resolve) {
              for (var i = 0; i < keys.length; i++) {
                pendingInputResolvers.set(keys[i], resolve);
              }
            });
          }
        };
        var work = (typeof fn.length === 'number' && fn.length >= 1) ? function () { return fn(emit); } : fn;
        return Promise.resolve().then(work).then(function (out) {
          if (out != null && typeof out === 'object' && 'message' in out && typeof out.message === 'string') {
            if (typeof onCompletedCb === 'function') onCompletedCb(out.message);
            emitMessage(out.message);
            emitCompleted();
            return;
          }
          var err = (out != null && typeof out === 'object' && 'error' in out) ? String(out.error) : 'Unknown error';
          if (typeof onFailedCb === 'function') {
            return Promise.resolve(onFailedCb(err)).catch(function () {}).then(function () {
              emitFailed(err, false);
            });
          }
          emitFailed(err, false);
        }).catch(function (err) {
          var msg = (err && err != null && typeof err.message === 'string') ? err.message : String(err);
          if (typeof onFailedCb === 'function') {
            return Promise.resolve(onFailedCb(msg)).catch(function () {}).then(function () {
              emitFailed(msg, false);
            });
          }
          emitFailed(msg, false);
        });
      }
    };
  }
  globalThis.session = session;
  globalThis.messageText = messageText;

  /**
   * Step Executor: thin wrapper over Rust-hosted __run_step_executor.
   * All FSM state, policy resolution, polymorphic narrowing, and multi-hop
   * coordination live in the Rust host. JS is only the call-through.
   */
  async function runGeneratedStepExecutor(stepExecutor, args, options) {
    if (typeof globalThis.__run_step_executor !== "function") {
      throw new Error("__run_step_executor host helper is not registered");
    }
    var argsJson = JSON.stringify((args != null && typeof args === "object") ? args : {});
    var optionsJson = (options != null && typeof options === "object") ? JSON.stringify(options) : null;
    var resultJson = await globalThis.__run_step_executor(String(stepExecutor), argsJson, optionsJson);
    return JSON.parse(resultJson);
  }
  globalThis.runGeneratedStepExecutor = runGeneratedStepExecutor;

  function assertNonEmptyString(value, field) {
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new Error(field + " must be a non-empty string");
    }
  }

  async function openA2aExecutionSession(_token) {
    if (typeof globalThis.__execution_session_invoke !== "function") {
      throw new Error("__execution_session_invoke host helper is not registered");
    }
    var invokeExecutionSession = async function(payload) {
      var encoded = await globalThis.__execution_session_invoke(JSON.stringify(payload));
      return JSON.parse(encoded);
    };
    var opened = await invokeExecutionSession({
      action: "open"
    });
    var sessionId = String(opened.sessionId);

    var api = {
      sessionId: sessionId,
      submitIntent: async function(intent) {
        await invokeExecutionSession({
          action: "submit_intent",
          session_id: sessionId,
          intent: intent
        });
        return this;
      },
      submitPlan: async function(incomingPlan) {
        await invokeExecutionSession({
          action: "submit_plan",
          session_id: sessionId,
          plan: incomingPlan
        });
        return this;
      },
      startStep: async function(stepId, evidenceText) {
        await invokeExecutionSession({
          action: "start_step",
          session_id: sessionId,
          step_id: stepId,
          evidence_text: evidenceText
        });
      },
      completeStep: async function(stepId, evidenceText) {
        await invokeExecutionSession({
          action: "complete_step",
          session_id: sessionId,
          step_id: stepId,
          evidence_text: evidenceText
        });
      },
      finish: async function() {
        return invokeExecutionSession({
          action: "finish",
          session_id: sessionId
        });
      },
      abort: async function(reason) {
        return invokeExecutionSession({
          action: "abort",
          session_id: sessionId,
          reason: reason
        });
      }
    };
    var currentRunSessions = globalThis.__a2a_current_execution_sessions;
    if (Array.isArray(currentRunSessions)) {
      currentRunSessions.push(api);
    }
    return api;
  }
  globalThis.openA2aExecutionSession = openA2aExecutionSession;

  function __chat_register(agent) {
    if (agent.run != null && typeof agent.run === 'function') {
      globalThis.onChatMessage = async function (message) {
        var s = session(message);
        var executionSessions = [];
        globalThis.__a2a_current_execution_sessions = executionSessions;
        var abortExecutionSessions = function(reason) {
          var tasks = [];
          for (var i = 0; i < executionSessions.length; i++) {
            var executionSession = executionSessions[i];
            if (executionSession != null && typeof executionSession.abort === "function") {
              tasks.push(Promise.resolve(executionSession.abort(reason)).catch(function() {}));
            }
          }
          if (tasks.length === 0) return Promise.resolve();
          return Promise.all(tasks).then(function() {});
        };
        try {
          s.onFailed(function(err) {
            var reason = (typeof err === "string" && err.trim().length > 0) ? err : "chat run failed";
            return abortExecutionSessions("runtime enforced abort: " + reason);
          });
          await s.run(function (emit) {
            return agent.run({ text: s.text() || '', message: message, emit: emit });
          });
        } finally {
          if (globalThis.__a2a_current_execution_sessions === executionSessions) {
            delete globalThis.__a2a_current_execution_sessions;
          }
        }
      };
    } else if (agent.onChatMessage != null) {
      globalThis.onChatMessage = agent.onChatMessage;
    }
    if (agent.onDispatch != null && typeof agent.onDispatch === 'function') {
      globalThis.onDispatch = agent.onDispatch;
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

  // extractDispatchMessages: return the messages array from a HostDispatchRequest.
  // Declared in baml-runtime.d.ts; implemented here as a QuickJS global.
  function extractDispatchMessages(request) {
    if (request == null) return [];
    var msgs = request.messages;
    if (!Array.isArray(msgs)) return [];
    return msgs;
  }
  globalThis.extractDispatchMessages = extractDispatchMessages;
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
        .map_err(|e| BamlBuilderError::InvalidArgumentWithSource {
            message: "A2A shim render error".into(),
            source: Box::new(GencoRenderError(e)),
        })
}
