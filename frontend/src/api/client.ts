// The typed API client. Routes, parameters, bodies and responses all come from the generated
// route table (src/generated/routes.ts), which is generated from the hub's Rust types.

import { ROUTES, type Routes } from "../generated";

const TOKEN_KEY = "botler.token";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** Raised (and the token forgotten) when the hub rejects the token. */
export class Unauthorized extends ApiError {}

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    // Storage disabled (private mode): the token lives for this tab only.
    return null;
  }
}

let token: string | null = storage()?.getItem(TOKEN_KEY) ?? null;
const unauthorizedHandlers: (() => void)[] = [];

export function getToken(): string | null {
  return token;
}

export function setToken(t: string | null): void {
  token = t;
  const s = storage();
  if (t) s?.setItem(TOKEN_KEY, t);
  else s?.removeItem(TOKEN_KEY);
}

export function onUnauthorized(f: () => void): void {
  unauthorizedHandlers.push(f);
}

type Has<T> = [T] extends [never] ? false : true;

/** The arguments a route needs: only the parts it has. */
export type Args<K extends keyof Routes> = (Routes[K]["params"] extends Record<string, never>
  ? { params?: undefined }
  : { params: Routes[K]["params"] }) &
  (Has<Routes[K]["query"]> extends true ? { query?: Routes[K]["query"] } : { query?: undefined }) &
  (Has<Routes[K]["body"]> extends true ? { body: Routes[K]["body"] } : { body?: undefined });

export function buildPath(path: string, params: Record<string, string> | undefined): string {
  return path.replace(/\{(\w+)\}/g, (_, name: string) => {
    const v = params?.[name];
    if (v === undefined) throw new Error(`missing path parameter ${name}`);
    return encodeURIComponent(v);
  });
}

export function buildQuery(query: Record<string, unknown> | undefined): string {
  if (!query) return "";
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) if (v !== undefined && v !== null) q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}

export async function call<K extends keyof Routes>(name: K, args: Args<K>): Promise<Routes[K]["response"]> {
  const route = ROUTES[name];
  const url =
    buildPath(route.path, args.params as Record<string, string> | undefined) +
    buildQuery(args.query as Record<string, unknown> | undefined);
  const headers: Record<string, string> = { authorization: `Bearer ${token ?? ""}` };
  let body: string | undefined;
  if (args.body !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(args.body);
  } else if (route.method !== "GET") {
    headers["content-type"] = "application/json";
    body = "{}";
  }
  let res: Response;
  try {
    res = await fetch(url, { method: route.method, headers, body });
  } catch (e) {
    throw new ApiError(0, `the hub is unreachable (${e instanceof Error ? e.message : String(e)})`);
  }
  const text = await res.text();
  let parsed: unknown = null;
  if (text) {
    try {
      parsed = JSON.parse(text);
    } catch {
      // Not JSON (e.g. a proxy error page): report the raw text below.
      parsed = null;
    }
  }
  if (res.status === 401) {
    setToken(null);
    for (const f of unauthorizedHandlers) f();
    throw new Unauthorized(401, errorText(parsed, text) || "your token is invalid or expired");
  }
  if (!res.ok) throw new ApiError(res.status, errorText(parsed, text) || res.statusText);
  return parsed as Routes[K]["response"];
}

function errorText(parsed: unknown, raw: string): string {
  if (parsed && typeof parsed === "object" && "error" in parsed && typeof parsed.error === "string") {
    return parsed.error;
  }
  return raw.slice(0, 300);
}
