// Server-side release authority. Input never selects a backend or supplies an
// origin. Native errors NEVER retry the legacy engine; transition is an explicit
// pre-cutover deployment stage, not an automatic fallback/shadow strategy.
import { createNativeSearchClient, type NativeSearchData, NativeSearchError } from './native-search-client';

export interface SiteSearchInput {
  projectId: string;
  query: string;
  language?: string;
  version?: string;
  limit?: number;
}
export interface SiteSearchData {
  hits: NativeSearchData['hits'];
  runtime: 'tantivy' | 'legacy' | 'hybrid' | 'shadow' | 'legacy-fallback';
  capabilities: { answer: boolean };
}
export type SearchAuthority = { authority: 'transition' } | { authority: 'native'; origin: string };

export function readSearchAuthority(environment: { NIBLEAF_AUTHORITY?: string; NIBLEAF_NATIVE_API_ORIGIN?: string }): SearchAuthority {
  const value = environment.NIBLEAF_AUTHORITY;
  if (value === undefined || value === 'transition') return { authority: 'transition' };
  if (value !== 'native') throw new Error('NIBLEAF_AUTHORITY must be transition or native');
  const origin = environment.NIBLEAF_NATIVE_API_ORIGIN;
  if (!origin) throw new Error('NIBLEAF_NATIVE_API_ORIGIN is required for native authority');
  // Constructor validates trusted operator config; never reflect its value.
  try {
    createNativeSearchClient(origin);
  } catch {
    throw new Error('NIBLEAF_NATIVE_API_ORIGIN must be a trusted HTTP(S) origin without credentials or path');
  }
  return { authority: 'native', origin };
}

export function publicSearchRuntime(profile: SearchAuthority) {
  return profile.authority === 'native'
    ? { authority: 'native' as const, runtime: 'tantivy' as const, answer: false as const, queryLimitUtf16: 200 }
    : { authority: 'transition' as const, runtime: 'legacy' as const, answer: null, queryLimitUtf16: null };
}

export async function executeSiteSearch(
  profile: SearchAuthority,
  input: SiteSearchInput,
  visitorHeaders: Headers,
  signal: AbortSignal | undefined,
  legacy: (input: SiteSearchInput) => Promise<SiteSearchData>,
  nativeFetch: typeof fetch = fetch,
): Promise<SiteSearchData> {
  if (profile.authority === 'native') {
    return createNativeSearchClient(profile.origin, nativeFetch)(input, visitorHeaders, signal);
  }
  return legacy(input);
}

export function requireAnswerAuthority(profile: SearchAuthority) {
  if (profile.authority === 'native') throw new NativeSearchError(503);
}
