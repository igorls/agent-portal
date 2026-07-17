import { Component, computed, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { BoardStore } from '../../core/state/board.store';
import { PortalCommands } from '../../core/ipc/commands';
import type { LedgerEntry } from '../../core/ipc/gen/LedgerEntry';
import {
  buildUsageReport,
  chartTicks,
  formatBytes,
  formatCount,
  type DayPoint,
  type NamedCount,
  type RangeKey,
} from './usage.analytics';
import { formatTokens, formatUsd } from './usage.pricing';

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

const RANGES: { id: RangeKey; label: string }[] = [
  { id: '7d', label: '7 days' },
  { id: '30d', label: '30 days' },
  { id: '90d', label: '90 days' },
  { id: 'all', label: 'All time' },
];

@Component({
  selector: 'app-usage-page',
  imports: [DatePipe],
  templateUrl: './usage-page.html',
  styleUrl: './usage-page.scss',
})
export class UsagePage {
  protected readonly store = inject(BoardStore);
  private readonly commands = inject(PortalCommands);

  protected readonly ranges = RANGES;
  protected readonly range = signal<RangeKey>('30d');
  protected readonly ledger = signal<LedgerEntry[]>([]);
  protected readonly ledgerError = signal<string | null>(null);
  /** Hovered activity bar key (day key). */
  protected readonly hoverDay = signal<string | null>(null);
  protected readonly hoverMigDay = signal<string | null>(null);

  protected readonly report = computed(() =>
    buildUsageReport(this.store.board(), this.ledger(), this.range())
  );

  protected readonly activityTicks = computed(() => chartTicks(this.report().timelineMax));
  protected readonly activityPeak = computed(() => {
    const tl = this.report().timeline;
    let best: DayPoint | null = null;
    for (const d of tl) {
      if (!best || d.count > best.count) best = d;
    }
    return best && best.count > 0 ? best : null;
  });
  protected readonly activityTotal = computed(() =>
    this.report().timeline.reduce((a, d) => a + d.count, 0)
  );

  protected readonly migTicks = computed(() => chartTicks(this.report().migrations.timelineMax));

  /** Max bar width basis for ranked lists. */
  protected readonly agentMax = computed(() =>
    Math.max(1, ...this.report().byAgent.map((b) => b.count))
  );
  protected readonly modelMax = computed(() =>
    Math.max(1, ...this.report().byModel.map((b) => b.count))
  );
  protected readonly projectMax = computed(() =>
    Math.max(1, ...this.report().byProject.map((b) => b.count))
  );
  protected readonly pairMax = computed(() =>
    Math.max(1, ...this.report().migrations.byPair.map((p) => p.count))
  );

  constructor() {
    if (!this.store.board()) void this.store.load();
    void this.loadLedger();
  }

  protected setRange(r: RangeKey): void {
    this.range.set(r);
    this.hoverDay.set(null);
    this.hoverMigDay.set(null);
  }

  protected async refresh(): Promise<void> {
    await Promise.all([this.store.refresh(), this.loadLedger()]);
  }

  protected agentLabel(id: string): string {
    return AGENT_LABELS[id] ?? id;
  }

  protected accent(agentId: string): string {
    return AGENT_ACCENTS[agentId] ?? '#7c8cf8';
  }

  protected modelAccent(index: number): string {
    const palette = ['#7c8cf8', '#5bbfa2', '#e8956a', '#b98ae0', '#48c6d9', '#f0b429', '#e88fa8'];
    return palette[index % palette.length];
  }

  protected barPct(count: number, max: number): number {
    return Math.max(count > 0 ? 4 : 0, Math.round((count / Math.max(1, max)) * 100));
  }

  /** Bar height as % of the y-axis top (nice tick max), not raw data max. */
  protected chartBarPct(count: number, ticks: number[]): number {
    const top = ticks[ticks.length - 1] || 1;
    if (count <= 0) return 0;
    return Math.max(3, Math.round((count / top) * 100));
  }

  protected fmtBytes(n: number): string {
    return formatBytes(n);
  }

  protected fmtCount(n: number): string {
    return formatCount(n);
  }

  protected fmtTokens(n: number): string {
    return formatTokens(n);
  }

  protected fmtUsd(n: number | null | undefined): string {
    return formatUsd(n);
  }

  protected sharePct(share: number): number {
    return Math.round(share * 100);
  }

  protected coveragePct(coverage: number): number {
    return Math.round(coverage * 100);
  }

  protected bucketTitle(b: NamedCount): string {
    const cost =
      b.estimatedCostUsd != null ? ` · ~${formatUsd(b.estimatedCostUsd)}` : '';
    return `${b.count} sessions · ${formatCount(b.messages)} msgs · ${formatTokens(b.estimatedTokens)} tok · ${formatBytes(b.bytes)}${cost}`;
  }

  /** Show x-axis labels sparsely on dense timelines. */
  protected showTick(i: number, total: number): boolean {
    if (total <= 10) return true;
    if (total <= 35) return i % 3 === 0 || i === total - 1;
    return i % 7 === 0 || i === total - 1;
  }

  protected onBarEnter(key: string): void {
    this.hoverDay.set(key);
  }

  protected onBarLeave(): void {
    this.hoverDay.set(null);
  }

  protected onMigBarEnter(key: string): void {
    this.hoverMigDay.set(key);
  }

  protected onMigBarLeave(): void {
    this.hoverMigDay.set(null);
  }

  protected hoveredPoint(points: DayPoint[], key: string | null): DayPoint | null {
    if (!key) return null;
    return points.find((p) => p.key === key) ?? null;
  }

  private async loadLedger(): Promise<void> {
    this.ledgerError.set(null);
    try {
      this.ledger.set(await this.commands.listActivity());
    } catch (e) {
      this.ledgerError.set(String(e));
    }
  }
}
