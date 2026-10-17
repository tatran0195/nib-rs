import { prisma } from '@nibleaf/database';
import { assertLegacyTenantErasureAllowed, eraseProjectOrganization } from './tenant-erasure';

/** Preserve ownership when possible; otherwise route sole-member workspace
 * removal through the same retained-store privacy erasure as the product UI. */
export const reassignOrDeleteOrgs = async (userId: string): Promise<void> => {
  const memberships = await prisma.member.findMany({ where: { userId }, select: { organizationId: true, role: true } });
  // Native shared-workspace departure has its own atomic/fenced route. Do not
  // mutate an earlier legacy membership before a later native erasure refusal.
  if (memberships.length > 0) await assertLegacyTenantErasureAllowed();
  for (const membership of memberships) {
    const members = await prisma.member.findMany({
      where: { organizationId: membership.organizationId },
      select: { id: true, userId: true, role: true },
      orderBy: { createdAt: 'asc' },
    });
    const others = members.filter((member) => member.userId !== userId);
    if (others.length === 0) {
      await eraseProjectOrganization(membership.organizationId);
    } else if (membership.role === 'owner' && !others.some((member) => member.role === 'owner') && others[0]) {
      await prisma.member.update({ where: { id: others[0].id }, data: { role: 'owner' } });
    }
  }
};
