import { useMutation, useQuery } from '@tanstack/react-query';
import { answerSiteFn, getSiteSearchRuntimeFn, searchSiteResultsFn } from '@/functions/site-search';
import { queryKeys } from './query-keys';

const requireSiteId = (siteId: string | undefined) => {
  if (!siteId) throw new Error('Site ID is required');
  return siteId;
};

export const useSiteSearchRuntime = () =>
  useQuery({
    queryKey: ['public', 'site-search-runtime'],
    queryFn: () => getSiteSearchRuntimeFn(),
    staleTime: 60000,
  });

export const useSiteSearch = (siteId: string | undefined, query: string, language?: string, version?: string, limit?: number, enabled = true) => {
  const runtime = useSiteSearchRuntime();
  return useQuery({
    queryKey: [...queryKeys.site.search(siteId ?? '', query, language, version, limit), runtime.data?.authority ?? 'unknown'],
    enabled: Boolean(runtime.isSuccess && enabled && siteId && query.trim()),
    staleTime: 0,
    gcTime: runtime.data?.authority === 'native' ? 0 : 300000,
    queryFn: ({ signal }) =>
      searchSiteResultsFn({
        signal,
        data: {
          projectId: requireSiteId(siteId),
          query,
          ...(language ? { language } : {}),
          ...(version ? { version } : {}),
          ...(limit ? { limit } : {}),
        },
      }),
  });
};

export const useAnswerSite = () =>
  useMutation({
    mutationFn: (input: { projectId: string; query: string; language?: string; version?: string }) => answerSiteFn({ data: input }),
  });
