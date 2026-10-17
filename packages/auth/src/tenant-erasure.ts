import { createJob, QueueNames } from '@nibleaf/bullmq';
import { keys as clickHouseKeys, clickHouseWritesEnabled, deleteProjectAnalytics, deleteProjectUsage } from '@nibleaf/clickhouse';
import { beginUsageDeletion, prisma } from '@nibleaf/database';

export class TenantUsageDeletionPendingError extends Error {
  constructor(readonly pendingCount: number) {
    super('Usage ingestion is still draining.');
    this.name = 'TenantUsageDeletionPendingError';
  }
}

export class TenantErasureProjectNotFoundError extends Error {
  constructor() {
    super('Tenant project not found.');
    this.name = 'TenantErasureProjectNotFoundError';
  }
}

/** Expansion migration introduces native retained files/objects/replicas that
 * the legacy synchronous cleanup cannot fence. Refuse BEFORE any usage fence,
 * provider/queue effect or relational mutation, not after a late FK failure. */
export class TenantNativeErasureRequiredError extends Error {
  constructor() {
    super('Native retained-store lifecycle is required before tenant deletion.');
    this.name = 'TenantNativeErasureRequiredError';
  }
}
export const assertLegacyTenantErasureAllowed = async (): Promise<void> => {
  const rows = await prisma.$queryRaw<Array<{ native: boolean }>>`SELECT to_regclass('public.rust_search_generation') IS NOT NULL AS native`;
  if (rows.length !== 1 || rows[0]?.native !== false) throw new TenantNativeErasureRequiredError();
};

/** Single fail-closed tenant erasure workflow used by product and Better Auth
 * deletion paths. The relational fence prevents post-tombstone usage writes. */
export const eraseProjectOrganization = async (organizationId: string, expectedProjectId?: string) => {
  await assertLegacyTenantErasureAllowed();
  const project = await prisma.project.findUnique({ where: { organizationId }, select: { id: true } });
  if (!project) {
    const marker = await prisma.usageStorageMarker.findUnique({ where: { organizationId }, select: { organizationId: true } });
    if (marker || expectedProjectId) throw new TenantErasureProjectNotFoundError();
    await prisma.organization.delete({ where: { id: organizationId } });
    return { organizationId, projectId: null };
  }
  if (expectedProjectId && project.id !== expectedProjectId) throw new TenantErasureProjectNotFoundError();

  const deletion = await beginUsageDeletion(organizationId, project.id);
  if (!deletion.exists) throw new TenantErasureProjectNotFoundError();
  if (deletion.pendingCount > 0) throw new TenantUsageDeletionPendingError(deletion.pendingCount);

  if (clickHouseWritesEnabled(clickHouseKeys().ANALYTICS_MODE) || deletion.hadStorageMarker) {
    await Promise.all([deleteProjectAnalytics(organizationId, project.id), deleteProjectUsage(organizationId, project.id)]);
  }
  await createJob(QueueNames.SEARCH, { name: 'delete-project', data: { projectId: project.id } }, { jobId: `search-delete-${project.id}` });
  await prisma.usageProviderCheckpoint.updateMany({
    where: { organizationId },
    data: { status: 'deletion_pending', hasError: false },
  });
  await prisma.organization.delete({ where: { id: organizationId } });
  return { organizationId, projectId: project.id };
};
