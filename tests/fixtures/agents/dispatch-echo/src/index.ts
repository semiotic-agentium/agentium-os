/// <reference path="./baml-runtime.d.ts" />
import type { HostDispatchAck, HostDispatchRequest, SessionResult } from "./baml-runtime";

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    return { message: "dispatch-echo does not handle A2A messages" };
  },

  onDispatch: async (request: HostDispatchRequest): Promise<HostDispatchAck> => {
    const messages = extractDispatchMessages(request);
    return {
      accepted: true,
      detail: `routing_key=${request.routing_key} messages=${messages.length}`,
    };
  },
});
