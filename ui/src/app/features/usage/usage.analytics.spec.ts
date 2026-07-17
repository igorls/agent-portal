import { describe, expect, it } from 'vitest';
import type { BoardSnapshot } from '../../core/ipc/gen/BoardSnapshot';
import {
  buildUsageReport,
  chartTicks,
  formatBytes,
  normalizeModel,
  type FlatSession,
} from './usage.analytics';
import {
  estimateCostUsd,
  estimateTokensFromBytes,
  formatUsd,
  rateForModel,
} from './usage.pricing';

function boardWith(sessions: Partial<FlatSession>[]): BoardSnapshot {
  const byAgent = new Map<string, typeof sessions>();
  for (const s of sessions) {
    const agent = s.agentId ?? 'claude-code';
    const list = byAgent.get(agent) ?? [];
    list.push(s);
    byAgent.set(agent, list);
  }
  return {
    generatedAt: new Date().toISOString(),
    feasibility: [],
    lanes: [...byAgent.entries()].map(([agentId, ss]) => ({
      agent: {
        id: agentId,
        displayName: agentId,
        capabilities: {
          storeKind: 'jsonl_per_session',
          read: 'full',
          writeNative: 'full',
          watch: true,
          launchResume: true,
          launchNew: true,
          contextTokens: null,
          writeConfidence: null,
          versionRangeTested: '',
          notes: [],
        },
        installation: null,
      },
      error: null,
      projects: [
        {
          key: 'proj',
          label: 'demo',
          cwdNormalized: 'p:/demo',
          sessions: ss.map((s, i) => ({
            agentId,
            nativeId: s.nativeId ?? `sess-${i}`,
            projectKey: 'proj',
            title: null,
            cwd: s.cwd ?? 'p:/demo',
            gitBranch: null,
            model: s.model ?? null,
            createdAt: s.createdAtMs ? new Date(s.createdAtMs).toISOString() : null,
            updatedAt: s.updatedAtMs ? new Date(s.updatedAtMs).toISOString() : null,
            messageCount: s.messageCount ?? 0,
            messageCountExact: s.messageCountExact ?? true,
            sizeBytes: s.sizeBytes ?? 0,
            storePath: '/tmp/x',
            maybeLive: s.maybeLive ?? false,
          })),
        },
      ],
    })),
  };
}

describe('usage.analytics', () => {
  it('normalizes model ids', () => {
    expect(normalizeModel('openai/gpt-4o')).toBe('gpt-4o');
    expect(normalizeModel('claude-sonnet-4-20250514')).toBe('claude-sonnet-4');
    expect(normalizeModel(null)).toBeNull();
  });

  it('formats bytes', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
  });

  it('aggregates agents and filters by range', () => {
    const now = Date.parse('2026-07-17T12:00:00Z');
    const board = boardWith([
      {
        agentId: 'claude-code',
        model: 'claude-sonnet-4',
        messageCount: 10,
        sizeBytes: 1000,
        updatedAtMs: now - 2 * 86_400_000,
      },
      {
        agentId: 'codex',
        model: 'gpt-5',
        messageCount: 4,
        sizeBytes: 500,
        updatedAtMs: now - 40 * 86_400_000,
      },
    ]);

    const week = buildUsageReport(board, [], '7d', now);
    expect(week.sessions).toBe(1);
    expect(week.byAgent).toHaveLength(1);
    expect(week.byAgent[0].key).toBe('claude-code');
    expect(week.messages).toBe(10);
    expect(week.estimatedTokens).toBe(estimateTokensFromBytes(1000));
    expect(week.estimatedCostUsd).not.toBeNull();
    expect(week.estimatedCostUsd!).toBeGreaterThan(0);

    const all = buildUsageReport(board, [], 'all', now);
    expect(all.sessions).toBe(2);
    expect(all.agents).toBe(2);
    expect(all.models).toBe(2);
    expect(all.byModel.map((m) => m.key).sort()).toEqual(['claude-sonnet-4', 'gpt-5']);
    expect(all.estimatedTokens).toBe(estimateTokensFromBytes(1500));
  });

  it('counts migrations by pair in range', () => {
    const now = Date.parse('2026-07-17T12:00:00Z');
    const ledger = [
      {
        id: '1',
        at: new Date(now - 86_400_000).toISOString(),
        sourceAgent: 'claude-code',
        sourceNativeId: 'a',
        sourcePath: '/a',
        targetAgent: 'codex',
        targetNativeId: 'b',
        artifacts: [],
        verifyGrade: 'exact' as const,
        undone: false,
      },
      {
        id: '2',
        at: new Date(now - 100 * 86_400_000).toISOString(),
        sourceAgent: 'codex',
        sourceNativeId: 'c',
        sourcePath: '/c',
        targetAgent: 'grok-build',
        targetNativeId: 'd',
        artifacts: [],
        verifyGrade: 'exact' as const,
        undone: true,
      },
    ];
    const r = buildUsageReport(null, ledger, '30d', now);
    expect(r.migrations.total).toBe(1);
    expect(r.migrations.byPair[0].source).toBe('claude-code');
    expect(r.migrations.byPair[0].target).toBe('codex');
  });

  it('builds nice chart ticks', () => {
    expect(chartTicks(1)[0]).toBe(0);
    expect(chartTicks(10).at(-1)!).toBeGreaterThanOrEqual(10);
    expect(chartTicks(47).at(-1)!).toBeGreaterThanOrEqual(47);
    expect(chartTicks(85)).toEqual([0, 20, 40, 60, 80, 100]);
    expect(chartTicks(85).at(-1)!).toBe(100);
  });

  it('timeline points carry full labels', () => {
    const now = Date.parse('2026-07-17T12:00:00Z');
    const board = boardWith([
      {
        agentId: 'claude-code',
        model: 'claude-sonnet-4',
        messageCount: 2,
        sizeBytes: 400,
        updatedAtMs: now,
      },
    ]);
    const r = buildUsageReport(board, [], '7d', now);
    expect(r.timeline).toHaveLength(7);
    expect(r.timeline.some((d) => d.count === 1)).toBe(true);
    expect(r.timeline.every((d) => d.fullLabel.length > 0)).toBe(true);
  });
});

describe('usage.pricing', () => {
  it('maps known models to rates', () => {
    expect(rateForModel('claude-opus-4-7')).toEqual({ inputPerM: 5, outputPerM: 25 });
    expect(rateForModel('claude-sonnet-4')).toEqual({ inputPerM: 3, outputPerM: 15 });
    expect(rateForModel('gpt-5.5')).toEqual({ inputPerM: 5, outputPerM: 30 });
    expect(rateForModel('unknown')).toBeNull();
    expect(rateForModel(null)).toBeNull();
  });

  it('estimates cost with agentic mix', () => {
    const rate = rateForModel('claude-sonnet-4')!;
    const cost = estimateCostUsd(1_000_000, rate);
    // 0.85 * 3 + 0.15 * 15 = 2.55 + 2.25 = 4.8
    expect(cost).toBeCloseTo(4.8, 5);
  });

  it('formats usd', () => {
    expect(formatUsd(null)).toBe('—');
    expect(formatUsd(0)).toBe('$0');
    expect(formatUsd(0.004)).toBe('<$0.01');
    expect(formatUsd(3.2)).toBe('$3.20');
  });
});
