// Server-side adapter selected only by the explicit native release authority.
// Construct once from trusted operator configuration, never an incoming URL.
import { z } from 'zod';

const selector = z
  .string()
  .refine((value) => new TextEncoder().encode(value).length <= 191)
  .optional();
export const nativeSearchInput = z
  .object({
    projectId: z
      .string()
      .min(1)
      .max(191)
      .refine((value) => value !== '.' && value !== '..' && !/[\/\\\u0000-\u001f]/.test(value)),
    query: z
      .string()
      .refine((value) => value.length <= 200, 'At most 200 UTF-16 code units')
      .default(''), // Explicit: installed Zod max counts scalars.
    language: selector,
    version: selector,
    limit: z.number().int().min(1).max(50).optional(),
  })
  .strict();
const hit = z
  .object({
    id: z.string(),
    title: z.string(),
    path: z.string(),
    description: z.string(),
    icon: z.string().optional(),
    snippet: z.string(),
    score: z.number().finite(),
    language: z.string(),
  })
  .strict();
const result = z
  .object({
    data: z
      .object({
        hits: z.array(hit).max(50),
        runtime: z.literal('tantivy'),
        capabilities: z.object({ answer: z.literal(false) }).strict(),
      })
      .strict(),
  })
  .strict();
export type NativeSearchData = z.infer<typeof result>['data'];

export class NativeSearchError extends Error {
  constructor(readonly status: number) {
    super(`Native search unavailable (${status})`);
    this.name = 'NativeSearchError';
  }
}

const MAX_RESPONSE_BYTES = 1024 * 1024;
async function rejectResponse(response: Response, status: number): Promise<never> {
  try {
    await response.body?.cancel();
  } catch {
    throw new NativeSearchError(503);
  }
  throw new NativeSearchError(status);
}

async function boundedJson(response: Response): Promise<unknown> {
  const declared = response.headers.get('content-length');
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > MAX_RESPONSE_BYTES)) return rejectResponse(response, 503);
  if (!response.body) throw new NativeSearchError(503);
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > MAX_RESPONSE_BYTES) {
        await reader.cancel();
        throw new NativeSearchError(503);
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } catch {
    throw new NativeSearchError(503);
  }
}

export function createNativeSearchClient(trustedOrigin: string, request: typeof fetch = fetch, options: { timeoutMs?: number } = {}) {
  const timeoutMs = options.timeoutMs ?? 8000;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 35000) throw new Error('Native search timeout must be 1..35000 milliseconds');
  const origin = new URL(trustedOrigin);
  if (
    !['https:', 'http:'].includes(origin.protocol) ||
    origin.username ||
    origin.password ||
    origin.pathname !== '/' ||
    origin.search ||
    origin.hash
  ) {
    throw new Error('Native search requires a trusted HTTP(S) origin without credentials or path');
  }
  return async (
    input: z.input<typeof nativeSearchInput>,
    visitorHeaders: Headers = new Headers(),
    signal?: AbortSignal,
  ): Promise<NativeSearchData> => {
    const parsed = nativeSearchInput.parse(input);
    const url = new URL(`/v1/public/sites/${encodeURIComponent(parsed.projectId)}/search`, origin);
    url.searchParams.set('q', parsed.query);
    if (parsed.language !== undefined) url.searchParams.set('lang', parsed.language);
    if (parsed.version !== undefined) url.searchParams.set('version', parsed.version);
    if (parsed.limit !== undefined) url.searchParams.set('limit', String(parsed.limit));
    const headers = new Headers({ accept: 'application/json' });
    for (const name of ['cookie', 'authorization']) {
      const value = visitorHeaders.get(name);
      if (value !== null) headers.set(name, value);
    }
    const deadline = AbortSignal.timeout(timeoutMs);
    const effectiveSignal = signal ? AbortSignal.any([signal, deadline]) : deadline;
    const response = await request(url, { headers, signal: effectiveSignal, cache: 'no-store', redirect: 'error', method: 'GET' });
    if (!response.ok) return rejectResponse(response, response.status); // NEVER retry a legacy engine.
    if (!response.headers.get('content-type')?.toLowerCase().startsWith('application/json')) return rejectResponse(response, 503);
    const vary = (response.headers.get('vary') ?? '')
      .toLowerCase()
      .split(',')
      .map((v) => v.trim());
    if (!response.headers.get('cache-control')?.includes('no-store') || !vary.includes('cookie') || !vary.includes('authorization'))
      return rejectResponse(response, 503);
    const decoded = result.safeParse(await boundedJson(response));
    if (!decoded.success) throw new NativeSearchError(503);
    return decoded.data.data;
  };
}
