// Tool calling using BAML's union type pattern
// Based on: https://boundaryml.com/blog/deepseek-r1-function-calling

enum MathOperation {
  Add @alias("+")
  Subtract @alias("-")
  Multiply @alias("*")
  Divide @alias("/")
}

class Expression {
  left int @description("The first number in the calculation")
  operation MathOperation @description("The mathematical operation to perform")
  right int @description("The second number in the calculation")
}

// Define tools as class types
class WeatherTool {
  tool_name "support/get_weather" @description("Use this tool to get the current weather forecast for a specific city.")
  location string @description("The city and state or city and country, e.g. San Francisco, CA or London, UK")
}

class CalculatorTool {
  tool_name "support/calculate" @description("Use this tool to perform mathematical calculations.")
  expression Expression @description("The mathematical expression to calculate")
}

// Union type for tool choice
type ToolChoice = WeatherTool | CalculatorTool

// Function that returns which tool to call
function ChooseTool(user_message: string) -> ToolChoice {
  client DeepSeekOpenRouter
  prompt #"
    The user said: {{ user_message }}

    Determine which tool they need.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}
