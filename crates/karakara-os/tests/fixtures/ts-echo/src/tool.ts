import { defineTool, Type } from "@karakara/tool";

const Input = Type.Object({
  message: Type.String(),
});

const Output = Type.Object({
  echoed: Type.String(),
});

export default defineTool(
  {
    name: "support/ts-echo",
    version: "0.1.0",
    description: "Echo the input message (e2e fixture).",
    sideEffect: "read",
    input: Input,
    output: Output,
    net: [],
  },
  async (input, ctx) => {
    if (input.message === "forbidden") {
      throw ctx.error("permission", "fixture denied the message");
    }
    return { echoed: input.message };
  },
);
