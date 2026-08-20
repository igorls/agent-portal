import { describe, expect, it } from 'vitest';
import type { Turn } from '../../core/ipc/gen/Turn';
import {
  displayRole,
  findMatches,
  groupAssistantWork,
  highlightSegments,
  isProseTurn,
  isToolResultOnly,
  isWorkOnlyRow,
  jsonSizeHint,
  lastProseRows,
  tailWindow,
  toViewRows,
  toolArgHint,
  toolNameFromDump,
  windowAround,
} from './session-preview.search';

function turn(id: string, text: string): Turn {
  return {
    id,
    parentId: null,
    role: 'user',
    timestamp: null,
    model: null,
    isMeta: false,
    blocks: [{ kind: 'text', text }],
    usage: null,
  };
}

describe('session-preview search', () => {
  it('finds turns by case-insensitive substring and builds a snippet', () => {
    const turns = [
      turn('a', 'Scaffold the importer'),
      turn('b', 'Ciclo Assist hallucination debugging on bentokit'),
      turn('c', 'unrelated'),
    ];
    const hits = findMatches(turns, 'HALLUCINATION');
    expect(hits).toHaveLength(1);
    expect(hits[0].turnId).toBe('b');
    expect(hits[0].snippet.toLowerCase()).toContain('hallucination');
  });

  it('ignores queries shorter than two characters', () => {
    expect(findMatches([turn('a', 'ab')], 'a')).toEqual([]);
  });

  it('highlights every occurrence without using regex', () => {
    const segs = highlightSegments('Read foo then Read bar', 'Read');
    expect(segs.filter((s) => s.hit).map((s) => s.text)).toEqual(['Read', 'Read']);
    expect(segs.map((s) => s.text).join('')).toBe('Read foo then Read bar');
  });

  it('windows around a hit and pins the tail for long sessions', () => {
    expect(windowAround(10, 30, 10)).toEqual({ start: 8, end: 18 });
    expect(tailWindow(2629, 100)).toEqual({ start: 2529, end: 2629 });
    expect(tailWindow(40, 100)).toEqual({ start: 0, end: 40 });
  });

  it('pulls a path-like hint out of tool args', () => {
    expect(toolArgHint({ file_path: 'ui/src/app/features/preview/session-preview.ts' })).toContain(
      'session-preview.ts',
    );
    expect(jsonSizeHint('abcd')).toBe('6 B');
  });

  it('does not treat Claude tool-result user records as user messages', () => {
    const resultTurn: Turn = {
      id: 'r1',
      parentId: 'a1',
      role: 'user',
      timestamp: null,
      model: null,
      isMeta: false,
      blocks: [{ kind: 'tool_result', call_id: 'c1', output: 'ok', is_error: false }],
      usage: null,
    };
    const callTurn: Turn = {
      id: 'a1',
      parentId: null,
      role: 'assistant',
      timestamp: null,
      model: 'claude-fable-5',
      isMeta: false,
      blocks: [
        {
          kind: 'tool_call',
          call_id: 'c1',
          name: 'Bash',
          arguments: { command: 'git status' },
        },
      ],
      usage: null,
    };
    expect(displayRole(resultTurn)).toBe('tool');
    expect(isToolResultOnly(resultTurn)).toBe(true);
    expect(isToolResultOnly(turn('u', 'please fix this'))).toBe(false);
    const rows = toViewRows([callTurn, resultTurn, turn('u', 'next prompt')], 0, 3);
    expect(rows[0].displayRole).toBe('assistant');
    expect(rows[1].continuation).toBe(true);
    expect(rows[1].displayRole).toBe('tool');
    expect(rows[2].displayRole).toBe('user');
    expect(rows[2].continuation).toBe(false);
  });

  it('groups sequential assistant work and hides tool-only turns when collapsed', () => {
    const bash = (id: string): Turn => ({
      id,
      parentId: null,
      role: 'assistant',
      timestamp: null,
      model: 'claude-fable-5',
      isMeta: false,
      blocks: [
        { kind: 'tool_call', call_id: id, name: 'Bash', arguments: { command: 'git status' } },
      ],
      usage: null,
    });
    const result = (id: string, parent: string): Turn => ({
      id,
      parentId: parent,
      role: 'user',
      timestamp: null,
      model: null,
      isMeta: false,
      blocks: [{ kind: 'tool_result', call_id: parent, output: 'ok', is_error: false }],
      usage: null,
    });
    const prose = (id: string, text: string): Turn => ({
      id,
      parentId: null,
      role: 'assistant',
      timestamp: null,
      model: 'claude-fable-5',
      isMeta: false,
      blocks: [{ kind: 'text', text }],
      usage: null,
    });
    const rows = toViewRows(
      [bash('a'), result('r', 'a'), prose('p', 'Rebase is safe.'), bash('b'), result('s', 'b')],
      0,
      5,
    );
    const groups = groupAssistantWork(rows);
    expect(groups).toHaveLength(1);
    expect(groups[0].kind).toBe('assistant');
    expect(groups[0].proseRows.map((r) => r.turn.id)).toEqual(['p']);
    expect(groups[0].toolCalls).toBe(2);
    expect(groups[0].summary).toContain('Bash');
    expect(isWorkOnlyRow(rows[0])).toBe(true);
    expect(isWorkOnlyRow(rows[2])).toBe(false);
  });

  it('treats Antigravity ⚙ tool dumps as work, not prose', () => {
    const dump: Turn = {
      id: 's233',
      parentId: null,
      role: 'assistant',
      timestamp: null,
      model: null,
      isMeta: false,
      blocks: [
        {
          kind: 'text',
          text: '⚙ grep_search: {"Query":"cz-sessions-footer","SearchPath":"p:/rioblocks/bentokit"}',
        },
      ],
      usage: null,
    };
    expect(toolNameFromDump(dump.blocks[0].kind === 'text' ? dump.blocks[0].text : '')).toBe(
      'grep_search',
    );
    expect(isProseTurn(dump)).toBe(false);
    const rows = toViewRows([dump, turn('u', 'please polish the footer')], 0, 2);
    expect(isWorkOnlyRow(rows[0])).toBe(true);
    const groups = groupAssistantWork(rows);
    expect(groups).toHaveLength(2);
    expect(groups[0].summary).toContain('grep_search');
    expect(groups[0].proseRows).toHaveLength(0);
  });

  it('chat mode keeps only the last prose reply in an assistant sequence', () => {
    const rows = toViewRows(
      [
        turn('u', 'please polish the footer'),
        {
          id: 'a1',
          parentId: null,
          role: 'assistant',
          timestamp: null,
          model: null,
          isMeta: false,
          blocks: [{ kind: 'text', text: 'Checking the stylesheet.' }],
          usage: null,
        },
        {
          id: 'a2',
          parentId: null,
          role: 'assistant',
          timestamp: null,
          model: null,
          isMeta: false,
          blocks: [{ kind: 'tool_call', call_id: 'c', name: 'grep_search', arguments: {} }],
          usage: null,
        },
        {
          id: 'a3',
          parentId: null,
          role: 'assistant',
          timestamp: null,
          model: null,
          isMeta: false,
          blocks: [{ kind: 'text', text: 'Updated the footer contrast.' }],
          usage: null,
        },
      ],
      0,
      4,
    );
    const groups = groupAssistantWork(rows);
    expect(groups).toHaveLength(2);
    expect(lastProseRows(groups[1]).map((r) => r.turn.id)).toEqual(['a3']);
  });
});
