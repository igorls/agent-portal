import { Component, computed, inject, signal } from '@angular/core';
import { PortalCommands } from '../../core/ipc/commands';
import { TauriService } from '../../core/ipc/tauri.service';
import type { BoardSnapshot } from '../../core/ipc/gen/BoardSnapshot';
import type { SessionSummary } from '../../core/ipc/gen/SessionSummary';

const AGENT_ACCENTS: Record<string, string> = {
  'claude-code': '#e8956a',
  codex: '#5bbfa2',
  opencode: '#b98ae0',
};

interface PeekRow {
  session: SessionSummary;
  accent: string;
  project: string;
}

/**
 * The quick-peek popup: a small always-on-top window shown from the tray so
 * you can glance at what's live without opening the full board. Reads the
 * cached snapshot only — instant, no store scan.
 */
@Component({
  selector: 'app-peek-panel',
  templateUrl: './peek-panel.html',
  styleUrl: './peek-panel.scss',
})
export class PeekPanel {
  private readonly commands = inject(PortalCommands);
  protected readonly tauri = inject(TauriService);

  private readonly board = signal<BoardSnapshot | null>(null);

  private readonly allRows = computed<PeekRow[]>(() => {
    const board = this.board();
    if (!board) return [];
    const rows: PeekRow[] = [];
    for (const lane of board.lanes) {
      for (const project of lane.projects) {
        for (const s of project.sessions) {
          rows.push({
            session: s,
            accent: AGENT_ACCENTS[s.agentId] ?? '#569cd6',
            project: project.label,
          });
        }
      }
    }
    return rows;
  });

  protected readonly live = computed(() => this.allRows().filter((r) => r.session.maybeLive));

  protected readonly recent = computed(() =>
    [...this.allRows()]
      .filter((r) => !r.session.maybeLive)
      .sort((a, b) => (b.session.updatedAt ?? '').localeCompare(a.session.updatedAt ?? ''))
      .slice(0, 6)
  );

  protected readonly agentCount = computed(
    () => this.board()?.lanes.filter((l) => l.agent.installation).length ?? 0
  );

  constructor() {
    void this.load();
  }

  private async load(): Promise<void> {
    try {
      this.board.set(await this.commands.getCachedBoard());
    } catch {
      /* leave empty; the tray still works */
    }
  }

  protected openBoard(): void {
    void this.tauri.showMainBoard();
  }

  protected timeAgo(iso: string | null): string {
    if (!iso) return '';
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return '';
    const s = Math.round((Date.now() - then) / 1000);
    if (s < 60) return 'now';
    const m = Math.round(s / 60);
    if (m < 60) return `${m}m`;
    const h = Math.round(m / 60);
    if (h < 24) return `${h}h`;
    return `${Math.round(h / 24)}d`;
  }
}
