/// <reference path="./baml-runtime.d.ts" />

type ChatMessageWithToken = ChatMessage & { __baml_invocation_token?: string };

declare function runReActLoopHost(
  token: string,
  opts: {
    planFunction: string;
    userMessage: string;
    maxSteps?: number;
    dedupe?: boolean;
  }
): Promise<string>;

const MAX_REACT_STEPS = 8;

async function onChatMessage(message: ChatMessageWithToken): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const text = s.text() || "unknown";
    const token = message.__baml_invocation_token;

    if (!token) {
      return { message: "Missing invocation token." };
    }

    const response = await runReActLoopHost(token, {
      planFunction: "ChooseClickUpAction",
      userMessage: text,
      maxSteps: MAX_REACT_STEPS,
      dedupe: true,
    });

    return {
      message: response || "ClickUp action completed but returned no data.",
    };
  });
}

__chat_register({ onChatMessage });
