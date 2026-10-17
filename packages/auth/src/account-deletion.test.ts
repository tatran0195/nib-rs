import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  memberFindMany: vi.fn(),
  memberUpdate: vi.fn(),
  eraseProjectOrganization: vi.fn(),
  assertLegacyTenantErasureAllowed: vi.fn(),
}));

vi.mock('@nibleaf/database', () => ({ prisma: { member: { findMany: mocks.memberFindMany, update: mocks.memberUpdate } } }));
vi.mock('./tenant-erasure', () => ({
  eraseProjectOrganization: mocks.eraseProjectOrganization,
  assertLegacyTenantErasureAllowed: mocks.assertLegacyTenantErasureAllowed,
}));

import { reassignOrDeleteOrgs } from './account-deletion';

describe('Better Auth account deletion workspace handling', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.assertLegacyTenantErasureAllowed.mockResolvedValue(undefined);
    mocks.memberFindMany.mockReset();
    mocks.memberFindMany
      .mockResolvedValueOnce([{ organizationId: 'org-a', role: 'owner' }])
      .mockResolvedValueOnce([{ id: 'member-a', userId: 'user-a', role: 'owner' }]);
    mocks.eraseProjectOrganization.mockResolvedValue(undefined);
  });

  it('refuses native-schema tenant-bearing legacy deletion before any earlier ownership/membership mutation', async () => {
    mocks.memberFindMany.mockReset().mockResolvedValueOnce([
      { organizationId: 'shared-org', role: 'owner' },
      { organizationId: 'sole-org', role: 'owner' },
    ]);
    mocks.assertLegacyTenantErasureAllowed.mockRejectedValueOnce(new Error('native retained lifecycle required'));
    await expect(reassignOrDeleteOrgs('user-a')).rejects.toThrow('native retained lifecycle');
    expect(mocks.memberUpdate).not.toHaveBeenCalled();
    expect(mocks.eraseProjectOrganization).not.toHaveBeenCalled();
  });
  it('routes a sole-member organization through the shared retained-store erasure workflow', async () => {
    await reassignOrDeleteOrgs('user-a');
    expect(mocks.eraseProjectOrganization).toHaveBeenCalledWith('org-a');
    expect(mocks.memberUpdate).not.toHaveBeenCalled();
  });

  it('fails account deletion closed when retained-store erasure cannot complete', async () => {
    mocks.eraseProjectOrganization.mockRejectedValue(new Error('usage ingestion is still draining'));
    await expect(reassignOrDeleteOrgs('user-a')).rejects.toThrow('usage ingestion is still draining');
  });
});
