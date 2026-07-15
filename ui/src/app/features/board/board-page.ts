import { Component, computed, inject, signal } from '@angular/core';
import { CdkDrag, CdkDragPreview, CdkDropList, CdkDragDrop } from '@angular/cdk/drag-drop';
import { BoardStore } from '../../core/state/board.store';
import { PortalCommands } from '../../core/ipc/commands';
import { SessionPreview } from '../preview/session-preview';
import { MigrationWizard, type MigrationRequest } from '../migration/migration-wizard';
import type { CanonicalSession } from '../../core/ipc/gen/CanonicalSession';
import type { SessionSummary } from '../../core/ipc/gen/SessionSummary';

const LANE_ACCENTS: Record<string, string> = {
  'claude-code': '#e8956a',
  'claude-cowork': '#d97757',
  codex: '#5bbfa2',
  opencode: '#b98ae0',
  antigravity: '#48c6d9',
  copilot: '#e88fa8',
};

const COLLAPSED_CARD_LIMIT = 6;
const RAIL_WIDTH_DEFAULT = 276;
const RAIL_WIDTH_MIN = 200;
const RAIL_WIDTH_MAX = 520;
const RAIL_WIDTH_KEY = 'portal.railWidth';

/** One agent's sessions for the selected project (a column in the detail). */
interface AgentTrack {
  agentId: string;
  displayName: string;
  installed: boolean;
  sessions: SessionSummary[];
}

/** A project, deduplicated across agents by shared identity (normalized cwd).
    The sidebar lists these; selecting one drives the detail. */
interface ProjectEntry {
  id: string;
  label: string;
  path: string | null;
  /** agents that actually have sessions here, in lane order (for the dots) */
  agentIds: string[];
  totalSessions: number;
  lastActivityMs: number;
  liveCount: number;
}

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

  /** per (agent, project) "show all cards" toggle within a track */
  protected readonly shownFull = signal<ReadonlySet<string>>(new Set());
  /** agents hidden via the filter chips (narrow the detail to a pair, etc.) */
  protected readonly hiddenAgents = signal<ReadonlySet<string>>(new Set());
  /** free-text filter over the project rail */
  protected readonly projectFilter = signal('');
  /** explicitly selected project; null falls back to the most-recent one */
  protected readonly selectedId = signal<string | null>(null);
  /** user-resizable width of the project rail, persisted across sessions */
  protected readonly railWidth = signal(RAIL_WIDTH_DEFAULT);

  protected readonly preview = signal<CanonicalSession | null>(null);
  protected readonly previewLoading = signal<string | null>(null);
  protected readonly previewError = signal<string | null>(null);

  protected readonly dragSource = signal<string | null>(null);
  protected readonly migration = signal<MigrationRequest | null>(null);

  /** project identity -> aggregated data, including per-agent session lists */
  private readonly projectIndex = computed(() => {
    const map = new Map<
      string,
      {
        id: string;
        label: string;
        path: string | null;
        perAgent: Map<string, SessionSummary[]>;
        totalSessions: number;
        lastActivityMs: number;
        liveCount: number;
      }
    >();
    for (const lane of this.store.board()?.lanes ?? []) {
      for (const project of lane.projects) {
        const id = project.cwdNormalized ?? project.key;
        let e = map.get(id);
        if (!e) {
          e = {
            id,
            label: project.label,
            path: project.cwdNormalized ?? null,
            perAgent: new Map(),
            totalSessions: 0,
            lastActivityMs: 0,
            liveCount: 0,
          };
          map.set(id, e);
        }
        if (e.label === 'unknown' && project.label !== 'unknown') e.label = project.label;
        const list = e.perAgent.get(lane.agent.id) ?? [];
        for (const s of project.sessions) {
          list.push(s);
          e.totalSessions++;
          if (s.maybeLive) e.liveCount++;
          if (!e.path && s.cwd) e.path = s.cwd;
          const t = s.updatedAt ? new Date(s.updatedAt).getTime() : 0;
          if (!Number.isNaN(t) && t > e.lastActivityMs) e.lastActivityMs = t;
        }
        e.perAgent.set(lane.agent.id, list);
      }
    }
    return map;
  });

  /** sidebar list: every project, most-recently-active first */
  protected readonly projects = computed<ProjectEntry[]>(() => {
    const lanes = this.store.board()?.lanes ?? [];
    const order = new Map(lanes.map((l, i) => [l.agent.id, i] as const));
    return [...this.projectIndex().values()]
      .map((e) => ({
        id: e.id,
        label: e.label,
        path: e.path,
        agentIds: [...e.perAgent.entries()]
          .filter(([, ss]) => ss.length > 0)
          .map(([a]) => a)
          .sort((a, b) => (order.get(a) ?? 99) - (order.get(b) ?? 99)),
        totalSessions: e.totalSessions,
        lastActivityMs: e.lastActivityMs,
        liveCount: e.liveCount,
      }))
      .sort((a, b) => b.lastActivityMs - a.lastActivityMs);
  });

  protected readonly filteredProjects = computed<ProjectEntry[]>(() => {
    const q = this.projectFilter().trim().toLowerCase();
    if (!q) return this.projects();
    return this.projects().filter(
      (p) => p.label.toLowerCase().includes(q) || (p.path ?? '').toLowerCase().includes(q)
    );
  });

  /** effective selection: explicit, else the most-recent project */
  protected readonly activeId = computed(() => this.selectedId() ?? this.projects()[0]?.id ?? null);

  protected readonly activeEntry = computed<ProjectEntry | null>(
    () => this.projects().find((p) => p.id === this.activeId()) ?? null
  );

  /** the detail's columns: every installed agent (empty ones are drop targets),
      plus any agent that has sessions here, minus chip-hidden agents */
  protected readonly activeTracks = computed<AgentTrack[]>(() => {
    const id = this.activeId();
    if (id == null) return [];
    const entry = this.projectIndex().get(id);
    const hidden = this.hiddenAgents();
    return (this.store.board()?.lanes ?? [])
      .filter((l) => !hidden.has(l.agent.id))
      .filter((l) => !!l.agent.installation || (entry?.perAgent.get(l.agent.id)?.length ?? 0) > 0)
      .map((l) => ({
        agentId: l.agent.id,
        displayName: l.agent.displayName,
        installed: !!l.agent.installation,
        sessions: entry?.perAgent.get(l.agent.id) ?? [],
      }));
  });

  protected readonly dropListIds = computed(() =>
    this.activeTracks().map((t) => this.dropListId(t.agentId))
  );

  constructor() {
    const saved = Number(localStorage.getItem(RAIL_WIDTH_KEY));
    if (saved >= RAIL_WIDTH_MIN && saved <= RAIL_WIDTH_MAX) this.railWidth.set(saved);
    void this.store.load();
  }

  /** Drag the divider between the rail and stage to resize the rail. */
  protected startRailResize(ev: MouseEvent): void {
    ev.preventDefault();
    const startX = ev.clientX;
    const startW = this.railWidth();
    document.body.style.userSelect = 'none';
    document.body.style.cursor = 'col-resize';
    const onMove = (e: MouseEvent) => {
      const w = Math.min(RAIL_WIDTH_MAX, Math.max(RAIL_WIDTH_MIN, startW + (e.clientX - startX)));
      this.railWidth.set(w);
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
      document.body.style.cursor = '';
      localStorage.setItem(RAIL_WIDTH_KEY, String(this.railWidth()));
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  }

  protected dropListId(agentId: string): string {
    return `track-${agentId}`;
  }

  protected accent(agentId: string): string {
    return LANE_ACCENTS[agentId] ?? '#569cd6';
  }

  protected selectProject(id: string): void {
    this.selectedId.set(id);
  }

  protected isSelected(id: string): boolean {
    return this.activeId() === id;
  }

  protected onFilterInput(event: Event): void {
    this.projectFilter.set((event.target as HTMLInputElement).value);
  }

  protected isAgentHidden(agentId: string): boolean {
    return this.hiddenAgents().has(agentId);
  }

  protected toggleAgent(agentId: string): void {
    const next = new Set(this.hiddenAgents());
    if (!next.delete(agentId)) next.add(agentId);
    this.hiddenAgents.set(next);
  }

  protected laneSessionCount(agentId: string): number {
    let n = 0;
    for (const e of this.projectIndex().values()) n += e.perAgent.get(agentId)?.length ?? 0;
    return n;
  }

  protected visibleCards(track: AgentTrack): SessionSummary[] {
    if (this.isShownFull(track.agentId)) return track.sessions;
    return track.sessions.slice(0, COLLAPSED_CARD_LIMIT);
  }

  protected isShownFull(agentId: string): boolean {
    return this.shownFull().has(`${agentId}::${this.activeId()}`);
  }

  protected toggleShownFull(agentId: string): void {
    const key = `${agentId}::${this.activeId()}`;
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

  protected onDropTrack(event: CdkDragDrop<AgentTrack>, track: AgentTrack): void {
    const source = event.item.data as SessionSummary;
    if (!source || source.agentId === track.agentId) return;
    const feasibility = this.feasibilityFor(source.agentId, track.agentId);
    if (!feasibility || (!feasibility.native && !feasibility.brief)) return;
    this.migration.set({
      source,
      targetAgent: track.agentId,
      targetLabel: track.displayName,
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
