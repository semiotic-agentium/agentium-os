import { defineTool, Type, type Static } from "@karakara/tool";

const Input = Type.Object({ message: Type.String() });
const Output = Type.Object({ echoed: Type.String() });

export default defineTool(
  {
    name: "support/ts-echo",
    version: "0.1.0",
    description: "Echo the input message (e2e fixture).",
    sideEffect: "read",
    input: Input,
    output: Output,
  },
  async (input: Static<typeof Input>, _ctx): Promise<Static<typeof Output>> => {
    return { echoed: input.message };
  },
);
