import { describe, expect, it, vi } from 'vitest';
import { executeSiteSearch, publicSearchRuntime, readSearchAuthority, requireAnswerAuthority, type SiteSearchData } from './site-search-transport';
const legacyData: SiteSearchData = { hits: [], runtime: 'legacy', capabilities: { answer: true } };
const nativeData = {
  hits: [{ id: 'page', title: 'Safe title', path: 'guide', description: 'summary', snippet: 'text', score: 1, language: 'en' }],
  runtime: 'tantivy',
  capabilities: { answer: false },
};
const response = () =>
  new Response(JSON.stringify({ data: nativeData }), {
    headers: { 'content-type': 'application/json', 'cache-control': 'private, no-store', vary: 'Cookie, Authorization' },
  });

describe('explicit site-search release authority', () => {
  it('retains the current transition caller only when explicitly/default transition', async () => {
    const legacy = vi.fn().mockResolvedValue(legacyData);
    const native = vi.fn<typeof fetch>();
    const data = await executeSiteSearch(readSearchAuthority({}), { projectId: 'project', query: 'term' }, new Headers(), undefined, legacy, native);
    expect(data).toEqual(legacyData);
    expect(legacy).toHaveBeenCalledTimes(1);
    expect(native).not.toHaveBeenCalled();
  });
  it('selects native transport and forwards the real visitor identity without legacy activity', async () => {
    const legacy = vi.fn();
    const request = vi.fn<typeof fetch>().mockResolvedValue(response());
    const data = await executeSiteSearch(
      readSearchAuthority({ NIBLEAF_AUTHORITY: 'native', NIBLEAF_NATIVE_API_ORIGIN: 'https://native.example' }),
      { projectId: 'project', query: 'term' },
      new Headers({ cookie: 'nibleaf_identity=opaque', 'x-actor-id': 'forged' }),
      undefined,
      legacy,
      request,
    );
    expect(data).toEqual(nativeData);
    expect(legacy).not.toHaveBeenCalled();
    expect(new Headers(request.mock.calls[0]![1]?.headers).get('cookie')).toBe('nibleaf_identity=opaque');
    expect(new Headers(request.mock.calls[0]![1]?.headers).has('x-actor-id')).toBe(false);
  });
  it.each([401, 404, 422, 503])('never falls back after native HTTP %s', async (status) => {
    const legacy = vi.fn();
    const request = vi.fn<typeof fetch>().mockResolvedValue(new Response('error', { status }));
    await expect(
      executeSiteSearch(
        { authority: 'native', origin: 'https://native.example' },
        { projectId: 'project', query: 'term' },
        new Headers(),
        undefined,
        legacy,
        request,
      ),
    ).rejects.toHaveProperty('status', status);
    expect(legacy).not.toHaveBeenCalled();
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('never falls back after network cancellation/failure', async () => {
    const legacy = vi.fn();
    const request = vi.fn<typeof fetch>().mockRejectedValue(new DOMException('aborted', 'AbortError'));
    await expect(
      executeSiteSearch(
        { authority: 'native', origin: 'https://native.example' },
        { projectId: 'project', query: 'term' },
        new Headers(),
        undefined,
        legacy,
        request,
      ),
    ).rejects.toHaveProperty('name', 'AbortError');
    expect(legacy).not.toHaveBeenCalled();
  });
  it.each(['', 'legacy', 'Native', 'shadow'])('rejects invalid authority %s instead of silently choosing legacy', (value) => {
    expect(() => readSearchAuthority({ NIBLEAF_AUTHORITY: value })).toThrow('NIBLEAF_AUTHORITY');
  });
  it('requires a trusted native origin and redacts invalid config values', () => {
    expect(() => readSearchAuthority({ NIBLEAF_AUTHORITY: 'native' })).toThrow('NIBLEAF_NATIVE_API_ORIGIN');
    expect(() => readSearchAuthority({ NIBLEAF_AUTHORITY: 'native', NIBLEAF_NATIVE_API_ORIGIN: 'https://secret:password@example.test' })).toThrow(
      /^NIBLEAF_NATIVE_API_ORIGIN must be a trusted/,
    );
  });
  it('publishes no origin or answer capability in native mode', () => {
    const meta = publicSearchRuntime({ authority: 'native', origin: 'https://private-native.example' });
    expect(meta).toEqual({ authority: 'native', runtime: 'tantivy', answer: false, queryLimitUtf16: 200 });
    expect(JSON.stringify(meta)).not.toContain('private-native');
    expect(() => requireAnswerAuthority({ authority: 'native', origin: 'https://native.example' })).toThrow();
    expect(() => requireAnswerAuthority({ authority: 'transition' })).not.toThrow();
  });
  it('enforces explicit UTF-16 policy before any native or legacy network request', async () => {
    const legacy = vi.fn();
    const request = vi.fn<typeof fetch>();
    await expect(
      executeSiteSearch(
        { authority: 'native', origin: 'https://native.example' },
        { projectId: 'project', query: '😀'.repeat(101) },
        new Headers(),
        undefined,
        legacy,
        request,
      ),
    ).rejects.toThrow();
    expect(legacy).not.toHaveBeenCalled();
    expect(request).not.toHaveBeenCalled();
  });
});
