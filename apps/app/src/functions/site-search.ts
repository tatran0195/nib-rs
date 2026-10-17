import { createServerFn } from '@tanstack/react-start';
import { getRequest, getRequestHeaders, setResponseHeader } from '@tanstack/react-start/server';
import { z } from 'zod';
import { getData } from '@/hooks/api/client-helpers';
import type { SearchAnswer, SiteSearchHit } from '@/hooks/api/types';
import { api } from '@/services/api';
import {
  executeSiteSearch,
  publicSearchRuntime,
  readSearchAuthority,
  requireAnswerAuthority,
  type SiteSearchData,
  type SiteSearchInput,
} from '@/lib/site-search-transport';

const searchSiteInput = z.object({
  projectId: z.string().min(1),
  query: z.string().trim().min(1).max(300),
  language: z.string().min(1).optional(),
  version: z.string().min(1).optional(),
  limit: z.number().int().min(1).max(50).optional(),
});

const searchResultInput = searchSiteInput.extend({ query: z.string().max(300) });

const answerSiteInput = searchSiteInput.omit({ limit: true }).extend({ query: z.string().trim().min(2).max(300) });

// Release authority is trusted server environment, never a browser argument.
// Native failure is fail-closed; there is no catch-and-retry legacy path.
const profile = () =>
  readSearchAuthority({
    NIBLEAF_AUTHORITY: process.env.NIBLEAF_AUTHORITY,
    NIBLEAF_NATIVE_API_ORIGIN: process.env.NIBLEAF_NATIVE_API_ORIGIN,
  });

export const getSiteSearchRuntimeFn = createServerFn({ method: 'GET' }).handler(async () => publicSearchRuntime(profile()));

async function searchResult(data: SiteSearchInput) {
  setResponseHeader('Cache-Control', 'private, no-store');
  setResponseHeader('Vary', 'Cookie, Authorization');
  return executeSiteSearch(profile(), data, new Headers(getRequestHeaders()), getRequest().signal, async (raw) => {
    const input = searchSiteInput.parse(raw);
    return getData<SiteSearchData>(
      await api.public.sites[':id'].search.$get({
        param: { id: input.projectId },
        query: {
          q: input.query,
          ...(input.limit ? { limit: String(input.limit) } : {}),
          ...(input.language ? { lang: input.language } : {}),
          ...(input.version ? { version: input.version } : {}),
        },
      }),
      'search',
    );
  });
}

// Preserve the old hits-only server-function export for any pre-cutover caller.
export const searchSiteFn = createServerFn({ method: 'GET' })
  .validator(searchSiteInput)
  .handler(async ({ data }) => (await searchResult(data)).hits);

export const searchSiteResultsFn = createServerFn({ method: 'GET' })
  .validator(searchResultInput)
  .handler(async ({ data }) => searchResult(data));

export const answerSiteFn = createServerFn({ method: 'POST' })
  .validator(answerSiteInput)
  .handler(async ({ data }) => {
    requireAnswerAuthority(profile());
    return getData<SearchAnswer>(
      await api.public.sites[':id'].answer.$post({
        param: { id: data.projectId },
        json: {
          q: data.query,
          ...(data.language ? { lang: data.language } : {}),
          ...(data.version ? { version: data.version } : {}),
        },
      }),
      'answer',
    );
  });
