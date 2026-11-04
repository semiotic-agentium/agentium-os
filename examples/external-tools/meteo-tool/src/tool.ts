import { request } from "node:https";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const TOOL_NAME = "meteo-tool";
const USER_AGENT = "karakara-meteo-tool/2.0.0";
const GEOCODING_ENDPOINT = "https://geocoding-api.open-meteo.com/v1/search";
const FORECAST_ENDPOINT = "https://api.open-meteo.com/v1/forecast";

const inputSchema = {
  location_query: z
    .string()
    .describe(
      "Free-form place query (city, country, region), e.g. 'Buenos Aires' or 'Athens, Greece'.",
    ),
  city: z
    .string()
    .optional()
    .describe("Optional city hint. Used when location_query is omitted."),
  country: z.string().optional().describe("Optional country hint, e.g. 'Panama'."),
  timezone: z
    .string()
    .default("auto")
    .describe("IANA timezone like Europe/Berlin, or auto."),
  hourly_limit: z
    .number()
    .int()
    .min(1)
    .max(48)
    .default(12)
    .describe("Maximum number of hourly datapoints to return (1..48)."),
};

const outputSchema = z.object({
  source: z.string(),
  fetched_at: z.string(),
  location: z.object({
    query: z.string(),
    name: z.string().nullable(),
    admin1: z.string().nullable(),
    country: z.string().nullable(),
    country_code: z.string().nullable(),
    latitude: z.number(),
    longitude: z.number(),
    timezone: z.string(),
  }),
  current: z.object({
    time: z.string().nullable(),
    temperature_2m: z.number().nullable(),
    wind_speed_10m: z.number().nullable(),
  }),
  hourly: z.array(
    z.object({
      time: z.string(),
      temperature_2m: z.number(),
      relative_humidity_2m: z.number(),
      wind_speed_10m: z.number(),
    }),
  ),
  location_candidates: z.array(
    z.object({
      name: z.string().nullable(),
      admin1: z.string().nullable(),
      country: z.string().nullable(),
      latitude: z.number().nullable(),
      longitude: z.number().nullable(),
    }),
  ),
});

type Input = {
  location_query: string;
  city?: string;
  country?: string;
  timezone?: string;
  hourly_limit?: number;
};

type GeocodingResult = {
  name?: string;
  admin1?: string;
  country?: string;
  country_code?: string;
  latitude?: number;
  longitude?: number;
};

type GeocodingResponse = { results?: GeocodingResult[] };

type ForecastResponse = {
  latitude?: number;
  longitude?: number;
  timezone?: string;
  current?: {
    time?: string;
    temperature_2m?: number;
    wind_speed_10m?: number;
  };
  hourly?: {
    time?: string[];
    temperature_2m?: number[];
    relative_humidity_2m?: number[];
    wind_speed_10m?: number[];
  };
};

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "unknown network error";
}

function requestJson<T>(url: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const requestHandle = request(
      url,
      { headers: { Accept: "application/json", "User-Agent": USER_AGENT } },
      (response) => {
        let body = "";
        response.setEncoding("utf8");
        response.on("data", (chunk: string) => {
          body += chunk;
        });
        response.on("end", () => {
          const status = response.statusCode ?? 0;
          if (status < 200 || status >= 300) {
            reject(new Error(`Open-Meteo request failed with HTTP ${status}`));
            return;
          }
          try {
            resolve(JSON.parse(body) as T);
          } catch (error) {
            reject(new Error(`Open-Meteo returned invalid JSON: ${errorMessage(error)}`));
          }
        });
      },
    );
    requestHandle.setTimeout(15_000, () => {
      requestHandle.destroy(new Error("Open-Meteo request timed out"));
    });
    requestHandle.on("error", (error) => reject(new Error(errorMessage(error))));
    requestHandle.end();
  });
}

async function getJson<T>(url: string): Promise<T> {
  let lastError = "unknown network error";
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      return await requestJson<T>(url);
    } catch (error) {
      lastError = errorMessage(error);
      if (attempt < 3) {
        await new Promise((resolve) => setTimeout(resolve, attempt * 250));
      }
    }
  }
  throw new Error(`Open-Meteo request failed after 3 attempts: ${lastError}`);
}

function clean(value: string | undefined): string {
  return value?.trim() ?? "";
}

function normalizeInput(input: Input) {
  let locationQuery = clean(input.location_query);
  const city = clean(input.city);
  const country = clean(input.country);

  if (!locationQuery) {
    if (city && country) locationQuery = `${city}, ${country}`;
    else if (city) locationQuery = city;
    else if (country) locationQuery = country;
  }
  if (!locationQuery) {
    throw new Error("provide location_query, or city/country");
  }

  const timezone = clean(input.timezone) || "auto";
  const hourlyLimit = input.hourly_limit ?? 12;
  if (!Number.isInteger(hourlyLimit) || hourlyLimit < 1 || hourlyLimit > 48) {
    throw new Error("hourly_limit must be between 1 and 48");
  }

  return { locationQuery, country: country || undefined, timezone, hourlyLimit };
}

async function resolveLocation(locationQuery: string, countryHint?: string) {
  let queryText = locationQuery;
  if (countryHint && !locationQuery.toLowerCase().includes(countryHint.toLowerCase())) {
    queryText = `${locationQuery}, ${countryHint}`;
  }

  const params = new URLSearchParams({
    name: queryText,
    count: "5",
    language: "en",
    format: "json",
  });
  const payload = await getJson<GeocodingResponse>(`${GEOCODING_ENDPOINT}?${params}`);
  const results = payload.results ?? [];
  if (results.length === 0) {
    throw new Error(`no location found for '${locationQuery}'`);
  }

  const candidates = results.slice(0, 3).map((row) => ({
    name: row.name ?? null,
    admin1: row.admin1 ?? null,
    country: row.country ?? null,
    latitude: row.latitude ?? null,
    longitude: row.longitude ?? null,
  }));
  return { best: results[0], candidates, rawCandidates: results.slice(0, 3) };
}

function candidateLabel(candidate: GeocodingResult): string {
  return [candidate.name, candidate.admin1, candidate.country].filter(Boolean).join(", ");
}

function queryIsSpecific(locationQuery: string): boolean {
  return locationQuery.includes(",") || locationQuery.trim().split(/\s+/).length >= 3;
}

function rejectAmbiguous(
  locationQuery: string,
  countryHint: string | undefined,
  candidates: GeocodingResult[],
): void {
  if (countryHint || candidates.length < 2 || queryIsSpecific(locationQuery)) return;
  const first = candidates[0];
  const second = candidates[1];
  const countryDiffers = (first.country ?? "") !== (second.country ?? "");
  const adminDiffers = (first.admin1 ?? "") !== (second.admin1 ?? "");
  if (!countryDiffers && !adminDiffers) return;

  const options = candidates
    .slice(0, 3)
    .map((candidate, index) => `${index + 1}) ${candidateLabel(candidate)}`)
    .join("; ");
  throw new Error(
    `ambiguous location '${locationQuery}'. Please clarify by city and country. Options: ${options}`,
  );
}

async function fetchForecast(
  latitude: number,
  longitude: number,
  timezone: string,
): Promise<ForecastResponse> {
  const params = new URLSearchParams({
    latitude: String(latitude),
    longitude: String(longitude),
    current: "temperature_2m,wind_speed_10m",
    hourly: "temperature_2m,relative_humidity_2m,wind_speed_10m",
    timezone,
    forecast_days: "1",
  });
  return getJson<ForecastResponse>(`${FORECAST_ENDPOINT}?${params}`);
}

function hourlyRows(hourly: NonNullable<ForecastResponse["hourly"]>, limit: number) {
  const times = hourly.time ?? [];
  const temperatures = hourly.temperature_2m ?? [];
  const humidity = hourly.relative_humidity_2m ?? [];
  const wind = hourly.wind_speed_10m ?? [];
  const count = Math.min(times.length, temperatures.length, humidity.length, wind.length, limit);

  return Array.from({ length: count }, (_, index) => ({
    time: times[index],
    temperature_2m: temperatures[index],
    relative_humidity_2m: humidity[index],
    wind_speed_10m: wind[index],
  }));
}

async function getMeteo(input: Input) {
  const normalized = normalizeInput(input);
  const { best, candidates, rawCandidates } = await resolveLocation(
    normalized.locationQuery,
    normalized.country,
  );
  rejectAmbiguous(normalized.locationQuery, normalized.country, rawCandidates);

  if (best.latitude === undefined || best.longitude === undefined) {
    throw new Error("resolved location is missing coordinates");
  }

  const forecast = await fetchForecast(best.latitude, best.longitude, normalized.timezone);
  const current = forecast.current ?? {};
  const location = {
    query: normalized.locationQuery,
    name: best.name ?? null,
    admin1: best.admin1 ?? null,
    country: best.country ?? null,
    country_code: best.country_code ?? null,
    latitude: forecast.latitude ?? best.latitude,
    longitude: forecast.longitude ?? best.longitude,
    timezone: forecast.timezone ?? normalized.timezone,
  };
  const output = {
    source: "open-meteo",
    fetched_at: new Date().toISOString(),
    location,
    current: {
      time: current.time ?? null,
      temperature_2m: current.temperature_2m ?? null,
      wind_speed_10m: current.wind_speed_10m ?? null,
    },
    hourly: hourlyRows(forecast.hourly ?? {}, normalized.hourlyLimit),
    location_candidates: candidates,
  };
  return outputSchema.parse(output);
}

const server = new McpServer({ name: TOOL_NAME, version: "2.0.0" });
server.registerTool(
  TOOL_NAME,
  {
    description: "Accurate Weather Forecasts for Any Location",
    inputSchema,
    outputSchema,
    annotations: { readOnlyHint: true, idempotentHint: true },
  },
  async (input) => {
    const output = await getMeteo(input);
    return {
      content: [{ type: "text", text: JSON.stringify(output) }],
      structuredContent: output,
    };
  },
);

await server.connect(new StdioServerTransport());
