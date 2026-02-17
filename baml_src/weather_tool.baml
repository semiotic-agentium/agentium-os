// BAML function that can use weather tool
function GetWeatherInfo(location: string) -> string {
  client OpenRouterGPT
  prompt #"
    Get the weather information for {{ location }}.
    Provide a friendly response about the current weather conditions.

    If you need to check the weather, you can use the get_weather tool.
    Otherwise, just provide a general friendly response about the location.
  "#
}
