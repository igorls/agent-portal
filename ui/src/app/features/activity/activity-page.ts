import { Component, computed, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { PortalCommands } from '../../core/ipc/commands';
import type { LedgerEntry } from '../../core/ipc/gen/LedgerEntry';

const AGENT_LABELS: Record<string, string> = {
  'claude-code': 'Claude Code',
  codex: 'Codex CLI',
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

  constructor() {
    void this.load();
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
