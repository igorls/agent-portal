import { Component, computed, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { listen } from '@tauri-apps/api/event';
import { PortalCommands } from '../../core/ipc/commands';
import type { LedgerEntry } from '../../core/ipc/gen/LedgerEntry';
import type { NamingReport } from '../../core/ipc/gen/NamingReport';
import type { NamingProgress } from '../../core/ipc/gen/NamingProgress';
import type { NamingEntry } from '../../core/ipc/gen/NamingEntry';

const AGENT_LABELS: Record<string, string> = {
  'claude-code': 'Claude Code',
  'claude-cowork': 'Claude Cowork',
  codex: 'Codex CLI',
  opencode: 'OpenCode',
  antigravity: 'Antigravity',
  copilot: 'GitHub Copilot',
  'grok-build': 'Grok Build',
  'factory-droid': 'Factory Droid',
  pi: 'Pi',
  junie: 'Junie',
};

const AGENT_ACCENTS: Record<string, string> = {
  'claude-code': '#e8956a',
  'claude-cowork': '#d97757',
  codex: '#5bbfa2',
  opencode: '#b98ae0',
  antigravity: '#48c6d9',
  copilot: '#e88fa8',
  'grok-build': '#f0b429',
  'factory-droid': '#ff6b4a',
  pi: '#6b9fff',
  junie: '#8b7cf6',
};

const MIGRATION_PAGE = 25;
const TITLES_PAGE = 12;

interface DayGroup {
  key: string;
  label: string;
  entries: LedgerEntry[];
}

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

  /** free-text filter over agents / session ids */
  protected readonly query = signal('');
  /** when true, hide already-undone migrations */
  protected readonly hideUndone = signal(true);
  /** how many filtered migrations to render (newest first) */
  protected readonly migrationLimit = signal(MIGRATION_PAGE);
  /** how many naming titles to render */
  protected readonly titlesLimit = signal(TITLES_PAGE);
  /** collapse the long titles list under the naming summary */
  protected readonly titlesOpen = signal(false);

  protected readonly isEmpty = computed(() => !this.loading() && this.entries().length === 0);

  protected readonly naming = signal<NamingReport | null>(null);
  protected readonly namingError = signal<string | null>(null);

  /** Ticks every second so the "next pass" countdown stays live. */
  protected readonly now = signal(Date.now());

  protected readonly activeCount = computed(
    () => this.entries().filter((e) => !e.undone).length
  );
  protected readonly undoneCount = computed(
    () => this.entries().filter((e) => e.undone).length
  );

  /** Migrations after search + undone filter (newest first — already from API). */
  protected readonly filteredEntries = computed(() => {
    const q = this.query().trim().toLowerCase();
    const hide = this.hideUndone();
    return this.entries().filter((e) => {
      if (hide && e.undone) return false;
      if (!q) return true;
      const hay = [
        e.sourceAgent,
        e.targetAgent,
        this.agentLabel(e.sourceAgent),
        this.agentLabel(e.targetAgent),
        e.sourceNativeId,
        e.targetNativeId,
        e.verifyGrade,
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  });

  protected readonly visibleEntries = computed(() =>
    this.filteredEntries().slice(0, this.migrationLimit())
  );

  protected readonly hiddenMigrationCount = computed(() =>
    Math.max(0, this.filteredEntries().length - this.visibleEntries().length)
  );

  protected readonly nextMigrationBatch = computed(() =>
    Math.min(this.hiddenMigrationCount(), MIGRATION_PAGE)
  );

  /** Day-grouped slice of the visible migrations. */
  protected readonly dayGroups = computed<DayGroup[]>(() => {
    const groups: DayGroup[] = [];
    const byKey = new Map<string, DayGroup>();
    for (const entry of this.visibleEntries()) {
      const d = new Date(entry.at);
      const key = Number.isNaN(d.getTime())
        ? 'unknown'
        : `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
      let g = byKey.get(key);
      if (!g) {
        g = { key, label: this.dayLabel(d, key), entries: [] };
        byKey.set(key, g);
        groups.push(g);
      }
      g.entries.push(entry);
    }
    return groups;
  });

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

  protected readonly visibleTitles = computed<NamingEntry[]>(() => {
    const list = this.naming()?.entries ?? [];
    return list.slice(0, this.titlesLimit());
  });

  protected readonly hiddenTitleCount = computed(() => {
    const total = this.naming()?.entries.length ?? 0;
    return Math.max(0, total - this.visibleTitles().length);
  });

  protected readonly nextTitleBatch = computed(() =>
    Math.min(this.hiddenTitleCount(), TITLES_PAGE)
  );

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
      // Keep the user's scroll window after undo/refresh.
      this.migrationLimit.update((n) => Math.max(n, MIGRATION_PAGE));
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

  protected accent(agentId: string): string {
    return AGENT_ACCENTS[agentId] ?? '#569cd6';
  }

  protected onQueryInput(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
    this.migrationLimit.set(MIGRATION_PAGE);
  }

  protected toggleHideUndone(): void {
    this.hideUndone.update((v) => !v);
    this.migrationLimit.set(MIGRATION_PAGE);
  }

  protected showMoreMigrations(): void {
    this.migrationLimit.update((n) => n + MIGRATION_PAGE);
  }

  protected showAllMigrations(): void {
    this.migrationLimit.set(this.filteredEntries().length);
  }

  protected showMoreTitles(): void {
    this.titlesLimit.update((n) => n + TITLES_PAGE);
  }

  protected showAllTitles(): void {
    this.titlesLimit.set(this.naming()?.entries.length ?? 0);
  }

  protected toggleTitles(): void {
    this.titlesOpen.update((v) => !v);
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

  private dayLabel(d: Date, key: string): string {
    if (key === 'unknown' || Number.isNaN(d.getTime())) return 'Unknown date';
    const today = new Date();
    const startOf = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diffDays = Math.round((startOf(today) - startOf(d)) / 86_400_000);
    if (diffDays === 0) return 'Today';
    if (diffDays === 1) return 'Yesterday';
    if (diffDays > 1 && diffDays < 7) {
      return d.toLocaleDateString(undefined, { weekday: 'long' });
    }
    return d.toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
      year: d.getFullYear() !== today.getFullYear() ? 'numeric' : undefined,
    });
  }
}
