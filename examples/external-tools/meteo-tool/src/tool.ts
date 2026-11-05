import {
  defineTool,
  Type,
  type Static,
  type ToolContext,
} from "@karakara/tool";

const USER_AGENT = "karakara-meteo-tool/2.0.0";
const GEOCODING_ENDPOINT = "https://geocoding-api.open-meteo.com/v1/search";
const FORECAST_ENDPOINT = "https://api.open-meteo.com/v1/forecast";
const NullableString = Type.Union([Type.String(), Type.Null()]);
const NullableNumber = Type.Union([Type.Number(), Type.Null()]);

const Input = Type.Object({
  location_query: Type.String({
    description:
      "Free-form place query (city, country, region), e.g. 'Buenos Aires' or 'Athens, Greece'.",
  }),
  city: Type.Optional(
    Type.String({
      description: "Optional city hint. Used when location_query is omitted.",
    }),
  ),
  country: Type.Optional(
    Type.String({ description: "Optional country hint, e.g. 'Panama'." }),
  ),
  timezone: Type.Optional(
    Type.String({ description: "IANA timezone like Europe/Berlin, or auto." }),
  ),
  hourly_limit: Type.Optional(
    Type.Integer({
      minimum: 1,
      maximum: 48,
      description: "Maximum number of hourly datapoints to return (1..48).",
    }),
  ),
});

const Output = Type.Object({
  source: Type.String(),
  fetched_at: Type.String(),
  location: Type.Object({
    query: Type.String(),
    name: NullableString,
    admin1: NullableString,
    country: NullableString,
    country_code: NullableString,
    latitude: Type.Number(),
    longitude: Type.Number(),
    timezone: Type.String(),
  }),
  current: Type.Object({
    time: NullableString,
    temperature_2m: NullableNumber,
    wind_speed_10m: NullableNumber,
  }),
  hourly: Type.Array(
    Type.Object({
      time: Type.String(),
      temperature_2m: Type.Number(),
      relative_humidity_2m: Type.Number(),
      wind_speed_10m: Type.Number(),
    }),
  ),
  location_candidates: Type.Array(
    Type.Object({
      name: NullableString,
      admin1: NullableString,
      country: NullableString,
      latitude: NullableNumber,
      longitude: NullableNumber,
    }),
  ),
});

type InputValue = Static<typeof Input>;

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

async function requestJson<T>(url: string, ctx: ToolContext): Promise<T> {
  const response = await ctx.fetch(url, {
    headers: { Accept: "application/json", "User-Agent": USER_AGENT },
    signal: AbortSignal.timeout(15_000),
  });
  if (!response.ok) {
    throw new Error(`Open-Meteo request failed with HTTP ${response.status}`);
  }
  try {
    return (await response.json()) as T;
  } catch (error) {
    throw new Error(`Open-Meteo returned invalid JSON: ${errorMessage(error)}`);
  }
}

async function getJson<T>(url: string, ctx: ToolContext): Promise<T> {
  let lastError = "unknown network error";
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      return await requestJson<T>(url, ctx);
    } catch (error) {
      lastError = errorMessage(error);
      if (attempt < 3) {
        await new Promise((resolve) => setTimeout(resolve, attempt * 250));
      }
    }
  }
  throw ctx.error(
    "transient",
    `Open-Meteo request failed after 3 attempts: ${lastError}`,
  );
}

function clean(value: string | undefined): string {
  return value?.trim() ?? "";
}

function normalizeInput(input: InputValue, ctx: ToolContext) {
  let locationQuery = clean(input.location_query);
  const city = clean(input.city);
  const country = clean(input.country);

  if (!locationQuery) {
    if (city && country) locationQuery = `${city}, ${country}`;
    else if (city) locationQuery = city;
    else if (country) locationQuery = country;
  }
  if (!locationQuery) {
    throw ctx.error(
      "invalid_argument",
      "provide location_query, or city/country",
    );
  }

  const timezone = clean(input.timezone) || "auto";
  const hourlyLimit = input.hourly_limit ?? 12;
  return {
    locationQuery,
    country: country || undefined,
    timezone,
    hourlyLimit,
  };
}

async function resolveLocation(
  locationQuery: string,
  countryHint: string | undefined,
  ctx: ToolContext,
) {
  let queryText = locationQuery;
  if (
    countryHint &&
    !locationQuery.toLowerCase().includes(countryHint.toLowerCase())
  ) {
    queryText = `${locationQuery}, ${countryHint}`;
  }

  const params = new URLSearchParams({
    name: queryText,
    count: "5",
    language: "en",
    format: "json",
  });
  const payload = await getJson<GeocodingResponse>(
    `${GEOCODING_ENDPOINT}?${params}`,
    ctx,
  );
  const results = payload.results ?? [];
  if (results.length === 0) {
    throw ctx.error(
      "invalid_argument",
      `no location found for '${locationQuery}'`,
    );
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
  return [candidate.name, candidate.admin1, candidate.country]
    .filter(Boolean)
    .join(", ");
}

function queryIsSpecific(locationQuery: string): boolean {
  return (
    locationQuery.includes(",") || locationQuery.trim().split(/\s+/).length >= 3
  );
}

function rejectAmbiguous(
  locationQuery: string,
  countryHint: string | undefined,
  candidates: GeocodingResult[],
  ctx: ToolContext,
): void {
  if (countryHint || candidates.length < 2 || queryIsSpecific(locationQuery))
    return;
  const first = candidates[0];
  const second = candidates[1];
  const countryDiffers = (first.country ?? "") !== (second.country ?? "");
  const adminDiffers = (first.admin1 ?? "") !== (second.admin1 ?? "");
  if (!countryDiffers && !adminDiffers) return;

  const options = candidates
    .slice(0, 3)
    .map((candidate, index) => `${index + 1}) ${candidateLabel(candidate)}`)
    .join("; ");
  throw ctx.error(
    "invalid_argument",
    `ambiguous location '${locationQuery}'. Please clarify by city and country. Options: ${options}`,
  );
}

async function fetchForecast(
  latitude: number,
  longitude: number,
  timezone: string,
  ctx: ToolContext,
): Promise<ForecastResponse> {
  const params = new URLSearchParams({
    latitude: String(latitude),
    longitude: String(longitude),
    current: "temperature_2m,wind_speed_10m",
    hourly: "temperature_2m,relative_humidity_2m,wind_speed_10m",
    timezone,
    forecast_days: "1",
  });
  return getJson<ForecastResponse>(`${FORECAST_ENDPOINT}?${params}`, ctx);
}

function hourlyRows(
  hourly: NonNullable<ForecastResponse["hourly"]>,
  limit: number,
) {
  const times = hourly.time ?? [];
  const temperatures = hourly.temperature_2m ?? [];
  const humidity = hourly.relative_humidity_2m ?? [];
  const wind = hourly.wind_speed_10m ?? [];
  const count = Math.min(
    times.length,
    temperatures.length,
    humidity.length,
    wind.length,
    limit,
  );

  return Array.from({ length: count }, (_, index) => ({
    time: times[index],
    temperature_2m: temperatures[index],
    relative_humidity_2m: humidity[index],
    wind_speed_10m: wind[index],
  }));
}

async function getMeteo(input: InputValue, ctx: ToolContext) {
  const normalized = normalizeInput(input, ctx);
  const { best, candidates, rawCandidates } = await resolveLocation(
    normalized.locationQuery,
    normalized.country,
    ctx,
  );
  rejectAmbiguous(
    normalized.locationQuery,
    normalized.country,
    rawCandidates,
    ctx,
  );

  if (best.latitude === undefined || best.longitude === undefined) {
    throw ctx.error("execution", "resolved location is missing coordinates");
  }

  const forecast = await fetchForecast(
    best.latitude,
    best.longitude,
    normalized.timezone,
    ctx,
  );
  const current = forecast.current ?? {};
  return {
    source: "open-meteo",
    fetched_at: new Date().toISOString(),
    location: {
      query: normalized.locationQuery,
      name: best.name ?? null,
      admin1: best.admin1 ?? null,
      country: best.country ?? null,
      country_code: best.country_code ?? null,
      latitude: forecast.latitude ?? best.latitude,
      longitude: forecast.longitude ?? best.longitude,
      timezone: forecast.timezone ?? normalized.timezone,
    },
    current: {
      time: current.time ?? null,
      temperature_2m: current.temperature_2m ?? null,
      wind_speed_10m: current.wind_speed_10m ?? null,
    },
    hourly: hourlyRows(forecast.hourly ?? {}, normalized.hourlyLimit),
    location_candidates: candidates,
  };
}

export default defineTool(
  {
    name: "dev/meteo-tool",
    version: "2.0.0",
    description: "Accurate weather forecasts for any location.",
    sideEffect: "read",
    input: Input,
    output: Output,
    net: ["geocoding-api.open-meteo.com:443", "api.open-meteo.com:443"],
  },
  getMeteo,
);
