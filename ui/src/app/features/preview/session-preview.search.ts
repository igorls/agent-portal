import type { Block } from '../../core/ipc/gen/Block';
import type { Turn } from '../../core/ipc/gen/Turn';
import type { JsonValue } from '../../core/ipc/gen/serde_json/JsonValue';

export const WINDOW_SIZE = 100;
export const MIN_QUERY = 2;
export const TEXT_FOLD = 1800;
export const JSON_FOLD = 24_000;

export type SearchMatch = {
  turnIndex: number;
  turnId: string;
  snippet: string;
};

export type HighlightSeg = {
  text: string;
  hit: boolean;
};

export type DisplayRole = 'user' | 'assistant' | 'tool' | 'system';

export type ViewRow = {
  turn: Turn;
  localIndex: number;
  displayRole: DisplayRole;
  /** Tool-result payload that Claude stored as a `user` turn — not a person. */
  continuation: boolean;
};

/** Claude (and some others) wrap tool_result in a user record. That is not a user message. */
export function isToolResultOnly(turn: Turn): boolean {
  const content = turn.blocks.filter((b) => b.kind !== 'meta');
  return content.length > 0 && content.every((b) => b.kind === 'tool_result');
}

export function hasToolCall(turn: Turn): boolean {
  return turn.blocks.some((b) => b.kind === 'tool_call');
}

export function displayRole(turn: Turn): DisplayRole {
  if (isToolResultOnly(turn)) return 'tool';
  switch (turn.role) {
    case 'user':
    case 'assistant':
    case 'tool':
      return turn.role;
    default:
      return 'system';
  }
}

export function toViewRows(all: Turn[], start: number, end: number): ViewRow[] {
  return all.slice(start, end).map((turn, i) => {
    const abs = start + i;
    const prev = abs > 0 ? all[abs - 1] : undefined;
    const resultOnly = isToolResultOnly(turn);
    return {
      turn,
      localIndex: i,
      displayRole: displayRole(turn),
      continuation: resultOnly && !!prev && hasToolCall(prev),
    };
  });
}

export type WorkGroup = {
  id: string;
  kind: 'user' | 'assistant';
  rows: ViewRow[];
  proseRows: ViewRow[];
  toolCalls: number;
  thinking: number;
  summary: string;
};

export function isProseTurn(turn: Turn): boolean {
  return turn.blocks.some((b) => {
    if (b.kind === 'compaction') return true;
    return b.kind === 'text' && b.text.trim().length > 0 && !toolNameFromDump(b.text);
  });
}

export function isWorkBlock(kind: Block['kind']): boolean {
  return kind === 'thinking' || kind === 'tool_call' || kind === 'tool_result';
}

export function isWorkOnlyRow(row: ViewRow): boolean {
  if (row.continuation) return true;
  if (row.displayRole === 'user') return false;
  return !isProseTurn(row.turn);
}

export function workSummary(rows: ViewRow[]): string {
  const tally = new Map<string, number>();
  let thinking = 0;
  for (const row of rows) {
    for (const block of row.turn.blocks) {
      if (block.kind === 'tool_call') {
        tally.set(block.name, (tally.get(block.name) ?? 0) + 1);
      } else if (block.kind === 'text') {
        const dumped = toolNameFromDump(block.text);
        if (dumped) tally.set(dumped, (tally.get(dumped) ?? 0) + 1);
      }
      if (block.kind === 'thinking') thinking += 1;
    }
  }
  const parts: string[] = [];
  for (const [name, count] of tally) {
    parts.push(count > 1 ? `${count}× ${name}` : name);
  }
  if (thinking) parts.push(thinking > 1 ? `${thinking}× thinking` : 'thinking');
  return parts.join(' · ');
}

export function groupAssistantWork(rows: ViewRow[]): WorkGroup[] {
  const groups: WorkGroup[] = [];
  let burst: ViewRow[] = [];

  const flushBurst = () => {
    if (!burst.length) return;
    groups.push(makeGroup(burst, 'assistant'));
    burst = [];
  };

  for (const row of rows) {
    if (row.displayRole === 'user' && !row.continuation) {
      flushBurst();
      groups.push(makeGroup([row], 'user'));
    } else {
      burst.push(row);
    }
  }
  flushBurst();
  return groups;
}

/** Last assistant prose turn in a burst — the reply after the tool loop. */
export function lastProseRows(group: WorkGroup): ViewRow[] {
  if (group.kind === 'user') return group.rows;
  const prose = group.proseRows;
  if (!prose.length) return [];
  return [prose[prose.length - 1]];
}

function makeGroup(rows: ViewRow[], kind: WorkGroup['kind']): WorkGroup {
  const proseRows = kind === 'user' ? rows : rows.filter((row) => !isWorkOnlyRow(row));
  let toolCalls = 0;
  let thinking = 0;
  for (const row of rows) {
    for (const block of row.turn.blocks) {
      if (block.kind === 'tool_call') toolCalls += 1;
      if (block.kind === 'thinking') thinking += 1;
    }
  }
  return {
    id: rows[0]?.turn.id ?? 'g',
    kind,
    rows,
    proseRows,
    toolCalls,
    thinking,
    summary: kind === 'assistant' ? workSummary(rows) : '',
  };
}

export function jsonPreview(value: JsonValue, cap = JSON_FOLD): string {
  try {
    const raw = JSON.stringify(value, null, 2) ?? '';
    if (raw.length <= cap) return raw;
    return `${raw.slice(0, cap)}\n… truncated (${raw.length.toLocaleString()} chars)`;
  } catch {
    return String(value);
  }
}

export function jsonSizeHint(value: JsonValue): string {
  try {
    const n = JSON.stringify(value)?.length ?? 0;
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  } catch {
    return '';
  }
}

export function toolArgHint(args: JsonValue): string {
  if (args == null) return '';
  if (typeof args === 'string') return clip(args, 52);
  if (typeof args !== 'object' || Array.isArray(args)) return '';
  const keys = [
    'file_path',
    'path',
    'target_file',
    'TargetFile',
    'AbsolutePath',
    'SearchPath',
    'command',
    'query',
    'Query',
    'pattern',
    'url',
    'description',
    'Instruction',
  ];
  for (const key of keys) {
    const v = args[key];
    if (typeof v === 'string' && v.trim()) return clip(v, 52);
  }
  return '';
}

export function blockHaystack(block: Block): string {
  switch (block.kind) {
    case 'text':
      return block.text;
    case 'thinking':
      return block.encrypted ? '' : block.text;
    case 'tool_call':
      return `${block.name}\n${toolArgHint(block.arguments)}\n${jsonPreview(block.arguments, 4000)}`;
    case 'tool_result':
      return jsonPreview(block.output, 8000);
    case 'compaction':
      return block.summary;
    case 'meta':
      return block.source_kind;
    default:
      return '';
  }
}

export function turnHaystack(turn: Turn): string {
  return turn.blocks.map(blockHaystack).filter(Boolean).join('\n');
}

export function indexHaystacks(turns: Turn[]): string[] {
  return turns.map(turnHaystack);
}

export function findMatches(turns: Turn[], query: string, haystacks?: string[]): SearchMatch[] {
  const needle = query.trim().toLowerCase();
  if (needle.length < MIN_QUERY) return [];
  const index = haystacks ?? indexHaystacks(turns);
  const out: SearchMatch[] = [];
  for (let i = 0; i < turns.length; i++) {
    const hay = index[i] ?? '';
    const at = hay.toLowerCase().indexOf(needle);
    if (at < 0) continue;
    out.push({
      turnIndex: i,
      turnId: turns[i].id,
      snippet: snippetAround(hay, at, needle.length),
    });
  }
  return out;
}

export function highlightSegments(text: string, query: string, maxHits = 24): HighlightSeg[] {
  const needle = query.trim();
  if (needle.length < MIN_QUERY || !text) return [{ text, hit: false }];
  const lower = text.toLowerCase();
  const q = needle.toLowerCase();
  const segs: HighlightSeg[] = [];
  let i = 0;
  let hits = 0;
  while (i < text.length && hits < maxHits) {
    const j = lower.indexOf(q, i);
    if (j < 0) break;
    if (j > i) segs.push({ text: text.slice(i, j), hit: false });
    segs.push({ text: text.slice(j, j + needle.length), hit: true });
    i = j + needle.length;
    hits++;
  }
  if (i < text.length) segs.push({ text: text.slice(i), hit: false });
  return segs.length ? segs : [{ text, hit: false }];
}

export function windowAround(index: number, total: number, size = WINDOW_SIZE): {
  start: number;
  end: number;
} {
  if (total <= size) return { start: 0, end: total };
  const lead = Math.floor(size * 0.25);
  let start = Math.max(0, index - lead);
  let end = start + size;
  if (end > total) {
    end = total;
    start = Math.max(0, end - size);
  }
  return { start, end };
}

export function tailWindow(total: number, size = WINDOW_SIZE): { start: number; end: number } {
  if (total <= size) return { start: 0, end: total };
  return { start: total - size, end: total };
}

function snippetAround(hay: string, at: number, len: number, radius = 42): string {
  const from = Math.max(0, at - radius);
  const to = Math.min(hay.length, at + len + radius);
  const padL = from > 0 ? '…' : '';
  const padR = to < hay.length ? '…' : '';
  return `${padL}${hay.slice(from, to).replace(/\s+/g, ' ')}${padR}`;
}

function clip(s: string, n: number): string {
  const t = s.replace(/\s+/g, ' ').trim();
  return t.length <= n ? t : `${t.slice(0, n - 1)}…`;
}

/** Antigravity (and similar) flattened a tool step into `⚙ name: {json}`. */
export function toolNameFromDump(text: string): string | null {
  const t = text.trim();
  const m = t.match(/^⚙\s+([A-Za-z][\w./-]{0,80})/);
  return m?.[1] ?? null;
}
