import type { BoardSnapshot } from '../../core/ipc/gen/BoardSnapshot';
import type { LedgerEntry } from '../../core/ipc/gen/LedgerEntry';
import type { SessionSummary } from '../../core/ipc/gen/SessionSummary';
import {
  estimateCostUsd,
  estimateTokensFromBytes,
  rateForModel,
} from './usage.pricing';

export type RangeKey = '7d' | '30d' | '90d' | 'all';

export interface FlatSession {
  agentId: string;
  nativeId: string;
  model: string | null;
  projectKey: string;
  projectLabel: string;
  cwd: string | null;
  createdAtMs: number | null;
  updatedAtMs: number | null;
  /** Activity timestamp used for range filtering (updated → created). */
  activityMs: number | null;
  messageCount: number;
  messageCountExact: boolean;
  sizeBytes: number;
  maybeLive: boolean;
  /** Rough tokens from store size (chars/4 heuristic). */
  estimatedTokens: number;
  /** USD estimate when model has a known list rate; else null. */
  estimatedCostUsd: number | null;
}

export interface NamedCount {
  key: string;
  label: string;
  count: number;
  messages: number;
  bytes: number;
  share: number;
  estimatedTokens: number;
  /** Sum of priced session costs; null when nothing in the bucket is priced. */
  estimatedCostUsd: number | null;
  /** Share of this bucket's tokens that have a price. */
  costCoverage: number;
}

export interface DayPoint {
  key: string;
  label: string;
  /** Full date for tooltips, e.g. "Jul 12". */
  fullLabel: string;
  count: number;
  messages: number;
  estimatedTokens: number;
}

export interface MigrationPair {
  source: string;
  target: string;
  count: number;
  active: number;
}

export interface UsageReport {
  range: RangeKey;
  generatedAt: string | null;
  /** Sessions after range filter. */
  sessions: number;
  /** Sessions on the full board (pre-filter). */
  sessionsAll: number;
  agents: number;
  models: number;
  projects: number;
  messages: number;
  /** True when any message count is approximate. */
  messagesApprox: boolean;
  bytes: number;
  live: number;
  undated: number;
  /** Rough total tokens (store-size heuristic). */
  estimatedTokens: number;
  /**
   * Estimated API spend for sessions whose model maps to a known rate.
   * null when no session in range has a priced model.
   */
  estimatedCostUsd: number | null;
  /** Fraction of estimated tokens that contributed to cost (0–1). */
  costCoverage: number;
  byAgent: NamedCount[];
  byModel: NamedCount[];
  byProject: NamedCount[];
  timeline: DayPoint[];
  timelineMax: number;
  migrations: {
    total: number;
    active: number;
    undone: number;
    byPair: MigrationPair[];
    timeline: DayPoint[];
    timelineMax: number;
  };
}

const DAY_MS = 86_400_000;

export function rangeMs(range: RangeKey, now = Date.now()): number | null {
  switch (range) {
    case '7d':
      return now - 7 * DAY_MS;
    case '30d':
      return now - 30 * DAY_MS;
    case '90d':
      return now - 90 * DAY_MS;
    case 'all':
      return null;
  }
}

export function flattenBoard(board: BoardSnapshot | null): FlatSession[] {
  if (!board) return [];
  const out: FlatSession[] = [];
  for (const lane of board.lanes) {
    for (const project of lane.projects) {
      for (const s of project.sessions) {
        out.push(flattenSession(s, project.label, project.key));
      }
    }
  }
  return out;
}

function flattenSession(
  s: SessionSummary,
  projectLabel: string,
  projectKey: string,
): FlatSession {
  const createdAtMs = parseMs(s.createdAt);
  const updatedAtMs = parseMs(s.updatedAt);
  const model = normalizeModel(s.model);
  const sizeBytes = s.sizeBytes ?? 0;
  const estimatedTokens = estimateTokensFromBytes(sizeBytes);
  return {
    agentId: s.agentId,
    nativeId: s.nativeId,
    model,
    projectKey,
    projectLabel: projectLabel || projectKey,
    cwd: s.cwd,
    createdAtMs,
    updatedAtMs,
    activityMs: updatedAtMs ?? createdAtMs,
    messageCount: s.messageCount ?? 0,
    messageCountExact: s.messageCountExact && s.messageCount != null,
    sizeBytes,
    maybeLive: s.maybeLive,
    estimatedTokens,
    estimatedCostUsd: estimateCostUsd(estimatedTokens, rateForModel(model)),
  };
}

export function buildUsageReport(
  board: BoardSnapshot | null,
  ledger: LedgerEntry[],
  range: RangeKey,
  now = Date.now(),
): UsageReport {
  const all = flattenBoard(board);
  const since = rangeMs(range, now);
  const undated = all.filter((s) => s.activityMs == null).length;
  const filtered = all.filter((s) => inRange(s.activityMs, since));

  const byAgent = rankBuckets(
    filtered,
    (s) => s.agentId,
    (key) => key,
  );
  const byModel = rankBuckets(
    filtered,
    (s) => s.model ?? 'unknown',
    (key) => (key === 'unknown' ? 'Unknown model' : key),
  );
  const byProject = rankBuckets(
    filtered,
    (s) => s.cwd ?? s.projectKey,
    (key, samples) => samples[0]?.projectLabel ?? key,
  ).slice(0, 12);

  const models = new Set(
    filtered.map((s) => s.model).filter((m): m is string => !!m),
  ).size;
  const projects = new Set(filtered.map((s) => s.cwd ?? s.projectKey)).size;
  const agents = new Set(filtered.map((s) => s.agentId)).size;
  const messages = filtered.reduce((a, s) => a + s.messageCount, 0);
  const messagesApprox = filtered.some((s) => s.messageCount > 0 && !s.messageCountExact);
  const bytes = filtered.reduce((a, s) => a + s.sizeBytes, 0);
  const live = filtered.filter((s) => s.maybeLive).length;
  const estimatedTokens = filtered.reduce((a, s) => a + s.estimatedTokens, 0);
  const { cost: estimatedCostUsd, coverage: costCoverage } = sumCosts(filtered);

  const timeline = buildTimeline(
    filtered.map((s) => ({
      ms: s.activityMs,
      messages: s.messageCount,
      tokens: s.estimatedTokens,
    })),
    range,
    now,
  );
  const timelineMax = Math.max(1, ...timeline.map((d) => d.count));

  const migFiltered = ledger.filter((e) => inRange(parseMs(e.at), since));
  const migTimeline = buildTimeline(
    migFiltered.map((e) => ({ ms: parseMs(e.at), messages: 0, tokens: 0 })),
    range,
    now,
  );
  const pairMap = new Map<string, MigrationPair>();
  for (const e of migFiltered) {
    const k = `${e.sourceAgent}→${e.targetAgent}`;
    let p = pairMap.get(k);
    if (!p) {
      p = { source: e.sourceAgent, target: e.targetAgent, count: 0, active: 0 };
      pairMap.set(k, p);
    }
    p.count++;
    if (!e.undone) p.active++;
  }
  const byPair = [...pairMap.values()].sort((a, b) => b.count - a.count).slice(0, 10);

  return {
    range,
    generatedAt: board?.generatedAt ?? null,
    sessions: filtered.length,
    sessionsAll: all.length,
    agents,
    models,
    projects,
    messages,
    messagesApprox,
    bytes,
    live,
    undated,
    estimatedTokens,
    estimatedCostUsd,
    costCoverage,
    byAgent,
    byModel,
    byProject,
    timeline,
    timelineMax,
    migrations: {
      total: migFiltered.length,
      active: migFiltered.filter((e) => !e.undone).length,
      undone: migFiltered.filter((e) => e.undone).length,
      byPair,
      timeline: migTimeline,
      timelineMax: Math.max(1, ...migTimeline.map((d) => d.count)),
    },
  };
}

function sumCosts(sessions: FlatSession[]): { cost: number | null; coverage: number } {
  let pricedTokens = 0;
  let totalTokens = 0;
  let cost = 0;
  let anyPriced = false;
  for (const s of sessions) {
    totalTokens += s.estimatedTokens;
    if (s.estimatedCostUsd != null) {
      anyPriced = true;
      cost += s.estimatedCostUsd;
      pricedTokens += s.estimatedTokens;
    }
  }
  return {
    cost: anyPriced ? cost : null,
    coverage: totalTokens > 0 ? pricedTokens / totalTokens : 0,
  };
}

function inRange(ms: number | null, since: number | null): boolean {
  if (since == null) return true;
  if (ms == null) return false;
  return ms >= since;
}

function parseMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  return Number.isNaN(t) ? null : t;
}

/** Collapse provider prefixes / paths into a short model id for grouping. */
export function normalizeModel(model: string | null | undefined): string | null {
  if (!model) return null;
  let m = model.trim();
  if (!m) return null;
  // strip common provider prefixes: "openai/gpt-4o" → "gpt-4o"
  const slash = m.lastIndexOf('/');
  if (slash >= 0 && slash < m.length - 1) m = m.slice(slash + 1);
  // drop trailing date tags that explode cardinality: "claude-…-20250514"
  m = m.replace(/-\d{8}$/, '');
  return m;
}

function rankBuckets(
  sessions: FlatSession[],
  keyOf: (s: FlatSession) => string,
  labelOf: (key: string, samples: FlatSession[]) => string,
): NamedCount[] {
  const map = new Map<
    string,
    {
      samples: FlatSession[];
      messages: number;
      bytes: number;
      tokens: number;
      cost: number;
      pricedTokens: number;
      anyPriced: boolean;
    }
  >();
  for (const s of sessions) {
    const key = keyOf(s);
    let b = map.get(key);
    if (!b) {
      b = {
        samples: [],
        messages: 0,
        bytes: 0,
        tokens: 0,
        cost: 0,
        pricedTokens: 0,
        anyPriced: false,
      };
      map.set(key, b);
    }
    b.samples.push(s);
    b.messages += s.messageCount;
    b.bytes += s.sizeBytes;
    b.tokens += s.estimatedTokens;
    if (s.estimatedCostUsd != null) {
      b.anyPriced = true;
      b.cost += s.estimatedCostUsd;
      b.pricedTokens += s.estimatedTokens;
    }
  }
  const total = sessions.length || 1;
  return [...map.entries()]
    .map(([key, b]) => ({
      key,
      label: labelOf(key, b.samples),
      count: b.samples.length,
      messages: b.messages,
      bytes: b.bytes,
      share: b.samples.length / total,
      estimatedTokens: b.tokens,
      estimatedCostUsd: b.anyPriced ? b.cost : null,
      costCoverage: b.tokens > 0 ? b.pricedTokens / b.tokens : 0,
    }))
    .sort((a, b) => b.count - a.count || b.messages - a.messages);
}

function buildTimeline(
  events: { ms: number | null; messages: number; tokens: number }[],
  range: RangeKey,
  now: number,
): DayPoint[] {
  const days = range === '7d' ? 7 : range === '30d' ? 30 : range === '90d' ? 90 : 30;
  // For "all", show last 30 days of activity density (still useful); full history is in totals.
  const start = startOfDay(now - (days - 1) * DAY_MS);
  const buckets = new Map<string, DayPoint>();
  for (let i = 0; i < days; i++) {
    const d = new Date(start + i * DAY_MS);
    const key = dayKey(d);
    buckets.set(key, {
      key,
      label: dayLabel(d, days),
      fullLabel: d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
      count: 0,
      messages: 0,
      estimatedTokens: 0,
    });
  }
  for (const e of events) {
    if (e.ms == null) continue;
    const key = dayKey(new Date(e.ms));
    const b = buckets.get(key);
    if (!b) continue;
    b.count++;
    b.messages += e.messages;
    b.estimatedTokens += e.tokens;
  }
  return [...buckets.values()];
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

function dayKey(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

function dayLabel(d: Date, spanDays: number): string {
  if (spanDays <= 7) {
    return d.toLocaleDateString(undefined, { weekday: 'short' });
  }
  if (spanDays <= 30) {
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }
  // 90d: sparse labels — component can still show every Nth
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = -1;
  do {
    v /= 1024;
    i++;
  } while (v >= 1024 && i < units.length - 1);
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function formatCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/**
 * Nice y-axis ticks for the activity chart: always [0, …, top] with top ≥ max.
 * Prefer 3–5 evenly spaced labels so the axis stays readable.
 */
export function chartTicks(max: number): number[] {
  const m = Math.max(1, max);
  const top = niceCeil(m);
  if (top <= 4) {
    const out: number[] = [];
    for (let v = 0; v <= top; v++) out.push(v);
    return out;
  }
  // aim for ~4 intervals (5 labels)
  const step = niceStep(top / 4);
  const ticks: number[] = [];
  for (let v = 0; v <= top + 1e-9; v += step) {
    ticks.push(Math.round(v));
  }
  if (ticks[ticks.length - 1] !== top) ticks.push(top);
  return ticks;
}

/** Smallest "nice" ceiling ≥ n (1/2/5 × 10^k). */
export function niceCeil(n: number): number {
  if (n <= 1) return 1;
  const pow = Math.pow(10, Math.floor(Math.log10(n)));
  const m = n / pow;
  if (m <= 1) return pow;
  if (m <= 2) return 2 * pow;
  if (m <= 5) return 5 * pow;
  return 10 * pow;
}

function niceStep(raw: number): number {
  if (raw <= 1) return 1;
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  const n = raw / pow;
  if (n <= 1.5) return pow;
  if (n <= 3) return 2 * pow;
  if (n <= 7) return 5 * pow;
  return 10 * pow;
}
