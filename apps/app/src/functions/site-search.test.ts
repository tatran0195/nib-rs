import { afterEach, beforeEach, expect, it, vi } from 'vitest';
const state = vi.hoisted(() => ({
  legacySearch: vi.fn(),
  legacyAnswer: vi.fn(),
  headers: new Headers(),
  responseHeaders: new Map<string, string>(),
  request: new Request('https://app.example/function'),
}));
vi.mock('@tanstack/react-start', () => ({
  createServerFn: () => {
    let validator: { parse: (input: unknown) => unknown } | undefined;
    const builder = {
      validator: (value: { parse: (input: unknown) => unknown }) => {
        validator = value;
        return builder;
      },
      handler: (handler: (value: { data: unknown }) => Promise<unknown>) => (options?: { data?: unknown }) =>
        handler({ data: validator ? validator.parse(options?.data) : options?.data }),
    };
    return builder;
  },
}));
vi.mock('@tanstack/react-start/server', () => ({
  getRequestHeaders: () => state.headers,
  getRequest: () => state.request,
  setResponseHeader: (name: string, value: string) => state.responseHeaders.set(name, value),
}));
vi.mock('@/services/api', () => ({
  api: { public: { sites: { ':id': { search: { $get: state.legacySearch }, answer: { $post: state.legacyAnswer } } } } },
}));
import { answerSiteFn, getSiteSearchRuntimeFn, searchSiteFn, searchSiteResultsFn } from './site-search';
const data = { hits: [], runtime: 'tantivy', capabilities: { answer: false } };
const success = () =>
  new Response(JSON.stringify({ data }), {
    headers: { 'content-type': 'application/json', 'cache-control': 'private, no-store', vary: 'Cookie, Authorization' },
  });
beforeEach(() => {
  vi.stubEnv('NIBLEAF_AUTHORITY', 'native');
  vi.stubEnv('NIBLEAF_NATIVE_API_ORIGIN', 'https://native.example');
  state.legacySearch.mockReset();
  state.legacyAnswer.mockReset();
  state.responseHeaders.clear();
  state.headers = new Headers({ cookie: 'nibleaf_identity=opaque', 'x-actor-id': 'forged' });
  state.request = new Request('https://app.example/function');
});
afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});
it('the actual server handler selects native, propagates visitor identity and marks the BFF response private', async () => {
  const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(success());
  vi.stubGlobal('fetch', fetch);
  const result = await searchSiteResultsFn({ data: { projectId: 'project', query: '界'.repeat(200) } });
  expect(result).toEqual(data);
  expect(state.legacySearch).not.toHaveBeenCalled();
  const [url, options] = fetch.mock.calls[0]!;
  expect(new URL(String(url)).searchParams.get('q')).toBe('界'.repeat(200));
  expect(new Headers(options?.headers).get('cookie')).toBe('nibleaf_identity=opaque');
  expect(new Headers(options?.headers).has('x-actor-id')).toBe(false);
  expect(state.responseHeaders.get('Cache-Control')).toBe('private, no-store');
  expect(state.responseHeaders.get('Vary')).toBe('Cookie, Authorization');
});
it('a native failure never calls legacy and keeps private response policy', async () => {
  vi.stubGlobal('fetch', vi.fn<typeof fetch>().mockResolvedValue(new Response('error', { status: 503 })));
  await expect(searchSiteResultsFn({ data: { projectId: 'project', query: 'term' } })).rejects.toHaveProperty('status', 503);
  expect(state.legacySearch).not.toHaveBeenCalled();
  expect(state.responseHeaders.get('Cache-Control')).toContain('no-store');
});
it('native authority disables the actual legacy answer handler even if called directly', async () => {
  await expect(answerSiteFn({ data: { projectId: 'project', query: 'question' } })).rejects.toHaveProperty('status', 503);
  expect(state.legacyAnswer).not.toHaveBeenCalled();
});
it('the runtime procedure sends capability policy but never the internal native origin', async () => {
  const result = await getSiteSearchRuntimeFn();
  expect(result).toEqual({ authority: 'native', runtime: 'tantivy', answer: false, queryLimitUtf16: 200 });
  expect(JSON.stringify(result)).not.toContain('native.example');
});
it('native UTF16 rejection happens before either network path', async () => {
  const fetch = vi.fn<typeof globalThis.fetch>();
  vi.stubGlobal('fetch', fetch);
  await expect(searchSiteResultsFn({ data: { projectId: 'project', query: '😀'.repeat(101) } })).rejects.toThrow();
  expect(fetch).not.toHaveBeenCalled();
  expect(state.legacySearch).not.toHaveBeenCalled();
});
it('explicit transition preserves the hits-only export and original trim contract', async () => {
  vi.stubEnv('NIBLEAF_AUTHORITY', 'transition');
  const old = { hits: [], runtime: 'legacy', capabilities: { answer: true } };
  state.legacySearch.mockResolvedValue(new Response(JSON.stringify({ data: old }), { headers: { 'content-type': 'application/json' } }));
  expect(await searchSiteFn({ data: { projectId: 'project', query: '  old query  ' } })).toEqual([]);
  expect(state.legacySearch).toHaveBeenCalledWith(expect.objectContaining({ query: { q: 'old query' } }));
});
