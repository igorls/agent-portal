import { Component, computed, inject, input, output, signal } from '@angular/core';
import { PortalCommands, type MigrationMode } from '../../core/ipc/commands';
import type { DryRunReport } from '../../core/ipc/gen/DryRunReport';
import type { MigrationResult } from '../../core/ipc/gen/MigrationResult';
import type { OllamaStatus } from '../../core/ipc/gen/OllamaStatus';
import type { SessionSummary } from '../../core/ipc/gen/SessionSummary';

export interface MigrationRequest {
  source: SessionSummary;
  targetAgent: string;
  targetLabel: string;
  native: boolean;
  brief: boolean;
}

type Stage = 'choose' | 'planning' | 'review' | 'executing' | 'done' | 'error';

@Component({
  selector: 'app-migration-wizard',
  templateUrl: './migration-wizard.html',
  styleUrl: './migration-wizard.scss',
})
export class MigrationWizard {
  readonly request = input.required<MigrationRequest>();
  readonly closed = output<boolean>();

  private readonly commands = inject(PortalCommands);

  protected readonly stage = signal<Stage>('choose');
  protected readonly mode = signal<MigrationMode>('native');
  protected readonly report = signal<DryRunReport | null>(null);
  protected readonly result = signal<MigrationResult | null>(null);
  protected readonly error = signal<string | null>(null);
  protected readonly launching = signal(false);

  protected readonly ollama = signal<OllamaStatus | null>(null);
  protected readonly enhance = signal(false);

  protected readonly censusRows = computed(() => {
    const c = this.report()?.census;
    if (!c) return [];
    return [
      { label: 'text', value: c.text },
      { label: 'thinking', value: c.thinking },
      { label: 'tool calls', value: c.toolCalls },
      { label: 'tool results', value: c.toolResults },
      { label: 'compaction', value: c.compaction },
      { label: 'meta', value: c.meta },
    ].filter((r) => r.value > 0);
  });

  /** native migration whose estimated size runs past the target's window */
  protected readonly oversized = computed(() => {
    const r = this.report();
    return (
      !!r &&
      r.kind === 'native' &&
      r.targetContextTokens != null &&
      r.estimatedTokens > r.targetContextTokens
    );
  });

  protected fmtTokens(n: number): string {
    return n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
  }

  /** Re-plan as a handoff brief (offered when a native migration is oversized). */
  protected switchToBrief(): void {
    if (this.request().brief) this.choose('brief');
  }

  constructor() {
    queueMicrotask(() => this.start());
  }

  private async start(): Promise<void> {
    const req = this.request();
    // Probe Ollama in the background so the brief toggle can appear.
    this.commands
      .checkOllama()
      .then((s) => {
        this.ollama.set(s);
        // Default the enhance toggle on when the default model is present.
        this.enhance.set(s.defaultPresent);
      })
      .catch(() => this.ollama.set(null));

    // Skip the mode step when only one path is feasible.
    if (req.native && !req.brief) {
      this.mode.set('native');
      void this.plan('native');
    } else if (req.brief && !req.native) {
      this.mode.set('brief');
      void this.plan('brief');
    } else {
      this.stage.set('choose');
    }
  }

  protected choose(mode: MigrationMode): void {
    this.mode.set(mode);
    void this.plan(mode);
  }

  protected async plan(mode: MigrationMode): Promise<void> {
    const req = this.request();
    this.stage.set('planning');
    this.error.set(null);
    try {
      const report = await this.commands.planMigration(
        req.source.agentId,
        req.source.nativeId,
        req.targetAgent,
        mode,
        { enhance: mode === 'brief' && this.enhance(), sourceStorePath: req.source.storePath }
      );
      this.report.set(report);
      this.stage.set('review');
    } catch (e) {
      this.error.set(String(e));
      this.stage.set('error');
    }
  }

  /** Re-plan the brief with the current enhance setting (toggled in review). */
  protected async toggleEnhance(): Promise<void> {
    this.enhance.set(!this.enhance());
    if (this.mode() === 'brief') {
      await this.plan('brief');
    }
  }

  protected async execute(): Promise<void> {
    const report = this.report();
    if (!report) return;
    this.stage.set('executing');
    try {
      this.result.set(await this.commands.executeMigration(report.planId));
      this.stage.set('done');
    } catch (e) {
      this.error.set(String(e));
      this.stage.set('error');
    }
  }

  protected async launch(): Promise<void> {
    const result = this.result();
    if (!result) return;
    this.launching.set(true);
    try {
      // The result carries the exact command to run — a native resume, or a
      // fresh session seeded to read the handoff brief.
      await this.commands.launchCommand(result.resumeCommand);
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.launching.set(false);
    }
  }

  protected close(): void {
    this.closed.emit(this.stage() === 'done');
  }
}
