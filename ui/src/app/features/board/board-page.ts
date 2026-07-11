import { Component, computed, inject, signal } from '@angular/core';
import { CdkDrag, CdkDragPreview, CdkDropList, CdkDragDrop } from '@angular/cdk/drag-drop';
import { BoardStore } from '../../core/state/board.store';
import { PortalCommands } from '../../core/ipc/commands';
import { SessionPreview } from '../preview/session-preview';
import { MigrationWizard, type MigrationRequest } from '../migration/migration-wizard';
import type { CanonicalSession } from '../../core/ipc/gen/CanonicalSession';
import type { Lane } from '../../core/ipc/gen/Lane';
import type { ProjectGroup } from '../../core/ipc/gen/ProjectGroup';
import type { SessionSummary } from '../../core/ipc/gen/SessionSummary';

const LANE_ACCENTS: Record<string, string> = {
  'claude-code': '#e8956a',
  codex: '#5bbfa2',
  opencode: '#b98ae0',
  antigravity: '#48c6d9',
  copilot: '#e88fa8',
};

const COLLAPSED_CARD_LIMIT = 5;

@Component({
  selector: 'app-board-page',
  imports: [SessionPreview, MigrationWizard, CdkDropList, CdkDrag, CdkDragPreview],
  templateUrl: './board-page.html',
  styleUrl: './board-page.scss',
})
export class BoardPage {
  protected readonly store = inject(BoardStore);
  private readonly commands = inject(PortalCommands);

  protected readonly collapsedLimit = COLLAPSED_CARD_LIMIT;

  /** open project sections, keyed by shared project identity (normalized cwd) */
  protected readonly openProjects = signal<ReadonlySet<string>>(new Set());
  /** per-lane "show all cards" within an open section */
  protected readonly shownFull = signal<ReadonlySet<string>>(new Set());

  protected readonly preview = signal<CanonicalSession | null>(null);
  protected readonly previewLoading = signal<string | null>(null);
  protected readonly previewError = signal<string | null>(null);

  protected readonly dragSource = signal<string | null>(null);
  protected readonly migration = signal<MigrationRequest | null>(null);

  protected readonly dropListIds = computed(() =>
    (this.store.board()?.lanes ?? []).map((l) => this.dropListId(l.agent.id))
  );

  /** project identities that appear in more than one agent lane */
  protected readonly linkedProjectIds = computed<ReadonlySet<string>>(() => {
    const counts = new Map<string, number>();
    for (const lane of this.store.board()?.lanes ?? []) {
      for (const project of lane.projects) {
        const id = this.projectId(project);
        counts.set(id, (counts.get(id) ?? 0) + 1);
      }
    }
    const linked = new Set<string>();
    for (const [id, n] of counts) if (n > 1) linked.add(id);
    return linked;
  });

  constructor() {
    void this.store.load();
  }

  protected dropListId(agentId: string): string {
    return `lane-${agentId}`;
  }

  protected accent(agentId: string): string {
    return LANE_ACCENTS[agentId] ?? '#569cd6';
  }

  /** Shared identity across lanes: normalized cwd, falling back to the key. */
  protected projectId(project: ProjectGroup): string {
    return project.cwdNormalized ?? project.key;
  }

  protected isProjectOpen(project: ProjectGroup): boolean {
    return this.openProjects().has(this.projectId(project));
  }

  protected isProjectLinked(project: ProjectGroup): boolean {
    return this.linkedProjectIds().has(this.projectId(project));
  }

  /** Toggle a project open. Because state is keyed by shared identity, the
      same folder opens (and highlights) across every agent lane at once. */
  protected toggleProject(project: ProjectGroup): void {
    const id = this.projectId(project);
    const next = new Set(this.openProjects());
    if (!next.delete(id)) next.add(id);
    this.openProjects.set(next);
  }

  protected isShownFull(agentId: string, projectKey: string): boolean {
    return this.shownFull().has(`${agentId}::${projectKey}`);
  }

  protected toggleShownFull(agentId: string, projectKey: string): void {
    const key = `${agentId}::${projectKey}`;
    const next = new Set(this.shownFull());
    if (!next.delete(key)) next.add(key);
    this.shownFull.set(next);
  }

  /** any migration (native or brief) possible from the dragged source into targetAgent? */
  protected canDropInto(targetAgent: string): boolean {
    const source = this.dragSource();
    if (!source || source === targetAgent) return false;
    return (this.store.board()?.feasibility ?? []).some(
      (f) => f.source === source && f.target === targetAgent && (f.native || f.brief)
    );
  }

  private feasibilityFor(source: string, target: string) {
    return (this.store.board()?.feasibility ?? []).find(
      (f) => f.source === source && f.target === target
    );
  }

  protected onDragStarted(agentId: string): void {
    this.dragSource.set(agentId);
  }

  protected onDragEnded(): void {
    this.dragSource.set(null);
  }

  protected onDrop(event: CdkDragDrop<Lane>, targetLane: Lane): void {
    const source = event.item.data as SessionSummary;
    if (!source || source.agentId === targetLane.agent.id) return;
    const feasibility = this.feasibilityFor(source.agentId, targetLane.agent.id);
    if (!feasibility || (!feasibility.native && !feasibility.brief)) return;
    this.migration.set({
      source,
      targetAgent: targetLane.agent.id,
      targetLabel: targetLane.agent.displayName,
      native: feasibility.native,
      brief: feasibility.brief,
    });
  }

  protected onMigrationClosed(performed: boolean): void {
    this.migration.set(null);
    if (performed) void this.store.refresh();
  }

  protected async openPreview(summary: SessionSummary): Promise<void> {
    this.previewLoading.set(summary.nativeId);
    this.previewError.set(null);
    try {
      const session = await this.commands.getSessionPreview(
        summary.agentId,
        summary.nativeId,
        summary.storePath
      );
      this.preview.set(session);
    } catch (e) {
      this.previewError.set(String(e));
    } finally {
      this.previewLoading.set(null);
    }
  }

  protected closePreview(): void {
    this.preview.set(null);
  }

  protected timeAgo(iso: string | null): string {
    if (!iso) return '—';
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return '—';
    const seconds = Math.round((Date.now() - then) / 1000);
    if (seconds < 60) return 'just now';
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.round(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.round(hours / 24);
    if (days < 30) return `${days}d ago`;
    const months = Math.round(days / 30);
    return `${months}mo ago`;
  }
}
