/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

const MAX_REACT_STEPS = 4;

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const userText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";

    await runGeneratedStepExecutor("ChooseMeteoAgentAction", {
      user_message: userText,
    }, { max_steps: MAX_REACT_STEPS });

    const message = await PresentMeteoAgentReply({ user_message: userText });
    return { message };
  },
});
