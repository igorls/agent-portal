import { Component, computed, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { listen } from '@tauri-apps/api/event';
import { PortalCommands } from '../../core/ipc/commands';
import type { LedgerEntry } from '../../core/ipc/gen/LedgerEntry';
import type { NamingReport } from '../../core/ipc/gen/NamingReport';
import type { NamingProgress } from '../../core/ipc/gen/NamingProgress';

const AGENT_LABELS: Record<string, string> = {
  'claude-code': 'Claude Code',
  codex: 'Codex CLI',
  opencode: 'OpenCode',
  antigravity: 'Antigravity',
  copilot: 'GitHub Copilot',
};

@Component({
  selector: 'app-activity-page',
  imports: [DatePipe],
  templateUrl: './activity-page.html',
  styleUrl: './activity-page.scss',
})
export class ActivityPage {
  private readonly commands = inject(PortalCommands);

  protected readonly entries = signal<LedgerEntry[]>([]);
  protected readonly loading = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly busy = signal<string | null>(null);
  protected readonly note = signal<string | null>(null);

  protected readonly isEmpty = computed(() => !this.loading() && this.entries().length === 0);

  protected readonly naming = signal<NamingReport | null>(null);
  protected readonly namingError = signal<string | null>(null);

  /** Ticks every second so the "next pass" countdown stays live. */
  protected readonly now = signal(Date.now());

  /** Human status line for the naming worker. */
  protected readonly namingStatus = computed<{ label: string; tone: string }>(() => {
    const n = this.naming();
    if (!n) return { label: 'Checking…', tone: 'idle' };
    if (!n.ollamaAvailable) return { label: 'Ollama offline', tone: 'off' };
    if (!n.modelPresent) return { label: `Model "${n.model}" not installed`, tone: 'off' };
    if (n.progress.active) return { label: 'Naming now', tone: 'active' };
    if (n.recent.pending + n.recent.stale > 0) return { label: 'Waiting for next pass', tone: 'idle' };
    return { label: 'Up to date', tone: 'ok' };
  });

  /** Recent-window coverage as a 0–100 fraction with a current title. */
  protected readonly coverage = computed(() => {
    const n = this.naming();
    if (!n || n.recent.total === 0) return 100;
    return Math.round((n.recent.named / n.recent.total) * 100);
  });

  /** Seconds until the next scheduled pass, or null when unknown/running. */
  protected readonly nextPassIn = computed<number | null>(() => {
    const n = this.naming();
    if (!n || n.progress.active || !n.progress.nextPassAt) return null;
    const secs = Math.round((Date.parse(n.progress.nextPassAt) - this.now()) / 1000);
    return secs > 0 ? secs : null;
  });

  constructor() {
    void this.load();
    void this.loadNaming();
    setInterval(() => this.now.set(Date.now()), 1000);
    // A full pass finished: counts and titles changed, reload everything.
    void listen<void>('titles-updated', () => void this.loadNaming()).catch(() => {
      /* Browser-only tests do not expose Tauri events. */
    });
    // Per-session worker heartbeat: patch just the live progress, no refetch.
    void listen<NamingProgress>('naming-progress', (e) => {
      const cur = this.naming();
      if (cur) this.naming.set({ ...cur, progress: e.payload });
    }).catch(() => {
      /* Browser-only tests do not expose Tauri events. */
    });
  }

  protected async load(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      this.entries.set(await this.commands.listActivity());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  protected async loadNaming(): Promise<void> {
    this.namingError.set(null);
    try {
      this.naming.set(await this.commands.namingStatus());
    } catch (e) {
      this.namingError.set(String(e));
    }
  }

  protected agentLabel(id: string): string {
    return AGENT_LABELS[id] ?? id;
  }

  protected async undo(entry: LedgerEntry, force = false): Promise<void> {
    this.busy.set(entry.id);
    this.note.set(null);
    try {
      const report = await this.commands.undoMigration(entry.id, force);
      if (report.skipped.length > 0 && report.removed.length === 0) {
        this.note.set(report.skipped.join(' · '));
      } else {
        this.note.set(
          `Removed ${report.removed.length} artifact${report.removed.length === 1 ? '' : 's'}` +
            (report.skipped.length ? ` · ${report.skipped.length} kept` : '')
        );
      }
      await this.load();
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.busy.set(null);
    }
  }
}
