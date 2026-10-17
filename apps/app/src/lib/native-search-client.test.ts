import { describe, expect, it, vi } from 'vitest';
import { z } from 'zod';
import { createNativeSearchClient, nativeSearchInput, NativeSearchError } from './native-search-client';
const data = {
  hits: [{ id: 'page', title: '<script>literal</script>', path: 'guide', description: 'summary', snippet: 'plain text', score: 1.5, language: 'en' }],
  runtime: 'tantivy',
  capabilities: { answer: false },
};
const ok = (body: unknown = { data }) =>
  new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json', 'cache-control': 'private, no-store', vary: 'Cookie, Authorization' },
  });
describe('staged native search adapter', () => {
  it('preserves fields, query names, credentials, abort, no-store and no-redirect policy', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(ok());
    const controller = new AbortController();
    const signal = controller.signal;
    const actual = await createNativeSearchClient('http://127.0.0.1:4311', request)(
      { projectId: 'project', query: '日本語 😀', language: 'ja', version: 'main', limit: 7 },
      new Headers({ cookie: 'nibleaf_reader_project=opaque', authorization: 'Bearer transition', host: 'hostile.example', 'x-actor-id': 'forged' }),
      signal,
    );
    expect(actual).toEqual(data);
    const [url, options] = request.mock.calls[0]!;
    expect(new URL(String(url)).pathname).toBe('/v1/public/sites/project/search');
    expect(Object.fromEntries(new URL(String(url)).searchParams)).toEqual({ q: '日本語 😀', lang: 'ja', version: 'main', limit: '7' });
    expect(Object.fromEntries(new Headers(options?.headers))).toEqual({
      accept: 'application/json',
      authorization: 'Bearer transition',
      cookie: 'nibleaf_reader_project=opaque',
    });
    expect(options).toMatchObject({ method: 'GET', cache: 'no-store', redirect: 'error' });
    expect(options?.signal?.aborted).toBe(false);
    controller.abort();
    expect(options?.signal?.aborted).toBe(true);
  });
  it('characterizes installed Zod max as scalar counting, not the selected native UTF-16 policy', () => {
    expect(z.string().max(200).safeParse('😀'.repeat(101)).success).toBe(true);
    expect(nativeSearchInput.safeParse({ projectId: 'id', query: '😀'.repeat(101) }).success).toBe(false);
  });
  it('uses UTF-16 units, accepts blank, and rejects unknown input', () => {
    expect(nativeSearchInput.parse({ projectId: 'id', query: '界'.repeat(200) }).query).toHaveLength(200);
    expect(nativeSearchInput.parse({ projectId: 'id' }).query).toBe('');
    expect(() => nativeSearchInput.parse({ projectId: 'id', query: '😀'.repeat(101) })).toThrow();
    expect(() => nativeSearchInput.parse({ projectId: 'id', allowed_page_ids: ['foreign'] })).toThrow();
    expect(() => nativeSearchInput.parse({ projectId: '../foreign' })).toThrow();
  });
  it.each([401, 404, 422, 503])('does not fall back or reflect private error text for %s', async (status) => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(new Response('private-query-token-sentinel', { status }));
    await expect(createNativeSearchClient('https://native.example', request)({ projectId: 'id', query: 'private' })).rejects.toEqual(
      new NativeSearchError(status),
    );
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('propagates transport cancellation without another engine request', async () => {
    const request = vi.fn<typeof fetch>().mockRejectedValue(new DOMException('aborted', 'AbortError'));
    await expect(createNativeSearchClient('https://native.example', request)({ projectId: 'id' })).rejects.toHaveProperty('name', 'AbortError');
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('rejects legacy runtime, answer capability, publication metadata and unsafe caching', async () => {
    for (const response of [
      ok({ data: { ...data, runtime: 'legacy' } }),
      ok({ data: { ...data, capabilities: { answer: true } } }),
      ok({ data: { ...data, generation_id: 'not-public' } }),
      new Response(JSON.stringify({ data })),
    ]) {
      await expect(
        createNativeSearchClient('https://native.example', vi.fn<typeof fetch>().mockResolvedValue(response))({ projectId: 'id' }),
      ).rejects.toBeInstanceOf(NativeSearchError);
    }
  });
  it('has a bounded transport deadline even without a caller signal', async () => {
    const request = vi.fn<typeof fetch>().mockImplementation(
      (_url, options) =>
        new Promise((_resolve, reject) => {
          options?.signal?.addEventListener('abort', () => reject(new DOMException('deadline', 'AbortError')));
        }),
    );
    await expect(createNativeSearchClient('https://native.example', request, { timeoutMs: 20 })({ projectId: 'id' })).rejects.toHaveProperty(
      'name',
      'AbortError',
    );
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('bounds actual response bytes without trusting content-length', async () => {
    const response = new Response(' '.repeat(1024 * 1024 + 1), {
      headers: { 'content-type': 'application/json', 'cache-control': 'private, no-store', vary: 'Cookie, Authorization' },
    });
    await expect(
      createNativeSearchClient('https://native.example', vi.fn<typeof fetch>().mockResolvedValue(response))({ projectId: 'id' }),
    ).rejects.toBeInstanceOf(NativeSearchError);
  });
  it.each(['file:///etc/passwd', 'https://user:password@native.example', 'https://native.example/another/path'])(
    'rejects invalid trusted origin %s',
    (origin) => {
      expect(() => createNativeSearchClient(origin)).toThrow();
    },
  );
});
