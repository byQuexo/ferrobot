/** Where traces go: presets for the two backends the guide covers, or any OTLP/HTTP endpoint. */
export type OtelBackend = "phoenix" | "langfuse" | "other";

export const OTEL_ENDPOINTS: Record<Exclude<OtelBackend, "other">, string> = {
  phoenix: "http://localhost:6006/v1/traces",
  langfuse: "https://cloud.langfuse.com/api/public/otel/v1/traces",
};

/** The preset a saved endpoint belongs to. */
export function backendOf(endpoint: string): OtelBackend {
  if (endpoint.includes("/api/public/otel")) return "langfuse";
  if (endpoint.includes(":6006/") || endpoint.includes("phoenix")) return "phoenix";
  return "other";
}

/** The `Authorization` value: Phoenix takes its API key as a bearer token, Langfuse its key pair as basic auth. */
export function authValue(backend: OtelBackend, key: string, secret = ""): string | null {
  const k = key.trim();
  if (backend === "phoenix") return k ? `Bearer ${k}` : null;
  if (backend === "langfuse") {
    const s = secret.trim();
    return k && s ? `Basic ${btoa(`${k}:${s}`)}` : null;
  }
  return k || null;
}
