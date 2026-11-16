// Limits form validation and the engine's effective limits (PLAN A.3 UpdateLimits).

import { asObject, optUint } from "./ipc";
import { formatBytes } from "./format";

export const MIN_ACTIVE = 1;
export const MAX_ACTIVE = 10;
export const MIN_CONNECTIONS = 1;
export const MAX_CONNECTIONS = 16;
/** The engine refuses caps below 1 KiB/s. */
export const MIN_RATE_BPS = 1024;

export type SpeedUnit = "off" | "KB/s" | "MB/s";

const UNIT_BYTES: Readonly<Record<Exclude<SpeedUnit, "off">, number>> = {
  "KB/s": 1024,
  "MB/s": 1024 * 1024,
};

/** What the user typed; everything is text until validated. */
export interface LimitsForm {
  maxActive: string;
  speedUnit: SpeedUnit;
  speedValue: string;
  connections: string;
}

/** The validated request values; `rate_limit` is the word `off` or bytes per second. */
export interface LimitsInput {
  max_active_downloads: number;
  rate_limit: "off" | number;
  max_connections: number;
}

export type LimitsErrors = Partial<Record<"maxActive" | "speed" | "connections", string>>;

export type LimitsValidation = { ok: true; value: LimitsInput } | { ok: false; errors: LimitsErrors };

function wholeNumber(text: string, min: number, max: number): number | undefined {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return undefined;
  const value = Number(trimmed);
  return value >= min && value <= max ? value : undefined;
}

function speedBytes(form: LimitsForm): number | "off" | undefined {
  if (form.speedUnit === "off") return "off";
  const text = form.speedValue.trim();
  if (!/^\d+(\.\d+)?$/.test(text)) return undefined;
  const bytes = Math.round(Number(text) * UNIT_BYTES[form.speedUnit]);
  return Number.isSafeInteger(bytes) && bytes >= MIN_RATE_BPS ? bytes : undefined;
}

/** Checks every field; the engine re-checks the same ranges. */
export function validateLimitsForm(form: LimitsForm): LimitsValidation {
  const active = wholeNumber(form.maxActive, MIN_ACTIVE, MAX_ACTIVE);
  const connections = wholeNumber(form.connections, MIN_CONNECTIONS, MAX_CONNECTIONS);
  const speed = speedBytes(form);
  const errors: LimitsErrors = {
    ...(active === undefined ? { maxActive: `Active downloads must be a whole number from ${MIN_ACTIVE} to ${MAX_ACTIVE}.` } : {}),
    ...(speed === undefined ? { speed: "Speed limit must be at least 1 KB/s, or choose Off." } : {}),
    ...(connections === undefined
      ? { connections: `Connections must be a whole number from ${MIN_CONNECTIONS} to ${MAX_CONNECTIONS}.` }
      : {}),
  };
  if (active === undefined || speed === undefined || connections === undefined) return { ok: false, errors };
  return { ok: true, value: { max_active_downloads: active, rate_limit: speed, max_connections: connections } };
}

/** Limits in force; an absent rate means unlimited. */
export interface EffectiveLimits {
  max_connections?: number;
  rate_limit_bps?: number;
  max_active_downloads?: number;
}

/** Parses the `{effective: {...}}` answer of `update_limits` / `get_limits`. */
export function parseLimitsResult(value: unknown): EffectiveLimits {
  const effective = asObject(asObject(value, "limits result")["effective"], "effective limits");
  const maxConnections = optUint(effective, "max_connections");
  const rate = optUint(effective, "rate_limit_bps");
  const maxActive = optUint(effective, "max_active_downloads");
  return {
    ...(maxConnections === undefined ? {} : { max_connections: maxConnections }),
    ...(rate === undefined ? {} : { rate_limit_bps: rate }),
    ...(maxActive === undefined ? {} : { max_active_downloads: maxActive }),
  };
}

function speedFields(bps: number | undefined): Pick<LimitsForm, "speedUnit" | "speedValue"> {
  if (bps === undefined) return { speedUnit: "off", speedValue: "" };
  if (bps % UNIT_BYTES["MB/s"] === 0) return { speedUnit: "MB/s", speedValue: String(bps / UNIT_BYTES["MB/s"]) };
  return { speedUnit: "KB/s", speedValue: String(Math.round((bps / UNIT_BYTES["KB/s"]) * 100) / 100) };
}

/** Form values that show what the engine reports. */
export function formFromEffective(limits: EffectiveLimits): LimitsForm {
  return {
    maxActive: limits.max_active_downloads === undefined ? "" : String(limits.max_active_downloads),
    connections: limits.max_connections === undefined ? "" : String(limits.max_connections),
    ...speedFields(limits.rate_limit_bps),
  };
}

/** One line per limit, as the engine reports them. */
export function describeLimits(limits: EffectiveLimits): string {
  const speed = limits.rate_limit_bps === undefined ? "Off" : `${formatBytes(limits.rate_limit_bps)}/s`;
  return [
    `Active downloads: ${limits.max_active_downloads ?? "unknown"}`,
    `Speed limit: ${speed}`,
    `Connections: ${limits.max_connections ?? "unknown"}`,
  ].join(" · ");
}
