import { API_PREFIX, TOKEN_STORAGE_KEY } from "./config";

/// RFC-9457 problem details shape returned by Gears on error.
export interface ProblemDetails {
  type?: string;
  title?: string;
  status?: number;
  detail?: string;
  trace_id?: string;
  [key: string]: unknown;
}

export class ApiError extends Error {
  status: number;
  problem?: ProblemDetails;

  constructor(status: number, message: string, problem?: ProblemDetails) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.problem = problem;
  }
}

function authHeader(): Record<string, string> {
  const token = localStorage.getItem(TOKEN_STORAGE_KEY);
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/// Issue an authenticated request against the Gears API and normalize
/// RFC-9457 problem responses into a user-friendly `ApiError`.
export async function apiFetch<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const res = await fetch(`${API_PREFIX}${path}`, {
    ...init,
    headers: {
      Accept: "application/json",
      ...authHeader(),
      ...(init.headers ?? {}),
    },
  });

  if (!res.ok) {
    let problem: ProblemDetails | undefined;
    try {
      problem = (await res.json()) as ProblemDetails;
    } catch {
      problem = undefined;
    }
    const message =
      problem?.detail || problem?.title || `Request failed (${res.status})`;
    throw new ApiError(res.status, message, problem);
  }

  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}
