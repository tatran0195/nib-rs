// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  authority: 'native' as 'native' | 'transition',
  actualNative: false,
  fetching: false,
  error: false,
  track: vi.fn(),
  answer: vi.fn(),
}));
vi.mock('@tanstack/react-pacer', () => ({ useDebouncedValue: (value: string) => [value] }));
vi.mock('@/providers/site-analytics-provider', () => ({ useSiteAnalytics: () => ({ track: state.track }) }));
vi.mock('@/hooks/api/site-search', () => ({
  useSiteSearchRuntime: () => ({ data: { authority: state.authority, runtime: state.authority === 'native' ? 'tantivy' : 'legacy' }, error: null }),
  useSiteSearch: () => ({
    data: {
      hits: [{ id: 'native-page', title: 'Native title', path: 'guide', description: 'summary', snippet: 'text', score: 1, language: 'en' }],
      runtime: state.actualNative || state.authority === 'native' ? 'tantivy' : 'legacy',
      capabilities: { answer: state.authority === 'transition' },
    },
    isFetching: state.fetching,
    isError: state.error,
    error: state.error ? new Error('denied') : null,
  }),
  useAnswerSite: () => ({ isPending: false, data: null, error: null, mutate: state.answer }),
}));
import { SiteSearch } from './site-search';
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
beforeEach(() => {
  state.authority = 'native';
  state.actualNative = false;
  state.fetching = false;
  state.error = false;
  state.track.mockReset();
  state.answer.mockReset();
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  Element.prototype.scrollIntoView = vi.fn();
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
const render = () =>
  act(async () => root.render(createElement(SiteSearch, { projectId: 'project', open: true, onOpenChange: vi.fn(), aiAnswers: true })));

it('native runtime hides stale configured AI answers and applies the selected UTF16 input bound', async () => {
  await render();
  expect(document.querySelector('[role="tablist"]')).toBeNull();
  expect(document.querySelector<HTMLInputElement>('[role="combobox"]')?.maxLength).toBe(200);
  expect(document.body.textContent).toContain('Native title');
  expect(state.answer).not.toHaveBeenCalled();
});
it('native runtime does not display a cached authorization result during revalidation or denial', async () => {
  state.fetching = true;
  await render();
  expect(document.body.textContent).not.toContain('Native title');
  state.fetching = false;
  state.error = true;
  await render();
  expect(document.body.textContent).not.toContain('Native title');
});
it('explicit transition preserves the old answer UI rather than silently retiring it before cutover', async () => {
  state.authority = 'transition';
  await render();
  expect(document.querySelector('[role="tablist"]')).not.toBeNull();
  expect(document.body.textContent).toContain('Ask AI');
});

it('a native result overrides stale transition metadata for answer/privacy controls', async () => {
  state.authority = 'transition';
  state.actualNative = true;
  await render();
  expect(document.querySelector('[role="tablist"]')).toBeNull();
  expect(document.querySelector<HTMLInputElement>('[role="combobox"]')?.maxLength).toBe(200);
});
