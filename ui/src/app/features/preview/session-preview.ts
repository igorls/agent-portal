import {
  afterNextRender,
  Component,
  computed,
  effect,
  ElementRef,
  HostListener,
  inject,
  Injector,
  input,
  output,
  signal,
  untracked,
  viewChild,
} from '@angular/core';
import { DatePipe } from '@angular/common';
import type { Block } from '../../core/ipc/gen/Block';
import type { CanonicalSession } from '../../core/ipc/gen/CanonicalSession';
import type { Turn } from '../../core/ipc/gen/Turn';
import {
  findMatches,
  groupAssistantWork,
  highlightSegments,
  indexHaystacks,
  isWorkBlock,
  jsonPreview,
  jsonSizeHint,
  lastProseRows,
  MIN_QUERY,
  tailWindow,
  TEXT_FOLD,
  toViewRows,
  toolArgHint,
  WINDOW_SIZE,
  windowAround,
  type HighlightSeg,
  type SearchMatch,
  type ViewRow,
  type WorkGroup,
} from './session-preview.search';

export type PreviewMode = 'chat' | 'work' | 'full';

/**
 * Read-only transcript renderer over the canonical IR. Long sessions stay
 * windowed (latest turns first) so a 2k-turn thread does not freeze the
 * panel; find-in-session jumps the window to a hit.
 */
@Component({
  selector: 'app-session-preview',
  imports: [DatePipe],
  templateUrl: './session-preview.html',
  styleUrl: './session-preview.scss',
})
export class SessionPreview {
  readonly session = input.required<CanonicalSession>();
  readonly closed = output<void>();

  private readonly injector = inject(Injector);
  private readonly searchInput = viewChild<ElementRef<HTMLInputElement>>('searchInput');
  private readonly scroller = viewChild<ElementRef<HTMLElement>>('scroller');

  protected readonly showMeta = signal(false);
  /** chat = user + last reply; work = all prose; full = every tool call */
  protected readonly mode = signal<PreviewMode>('chat');
  protected readonly query = signal('');
  protected readonly matchIndex = signal(0);
  protected readonly windowStart = signal(0);
  protected readonly expanded = signal<ReadonlySet<string>>(new Set());
  protected readonly textOpen = signal<ReadonlySet<string>>(new Set());
  protected readonly openGroups = signal<ReadonlySet<string>>(new Set());

  protected readonly filteredTurns = computed<Turn[]>(() => {
    const showMeta = this.showMeta();
    return this.session().timeline.filter((t) => showMeta || !t.isMeta);
  });

  protected readonly haystacks = computed(() => indexHaystacks(this.filteredTurns()));

  protected readonly matches = computed<SearchMatch[]>(() =>
    findMatches(this.filteredTurns(), this.query(), this.haystacks()),
  );

  protected readonly activeMatch = computed(() => {
    const hits = this.matches();
    if (!hits.length) return null;
    return hits[this.matchIndex() % hits.length] ?? null;
  });

  protected readonly windowEnd = computed(() => {
    const n = this.filteredTurns().length;
    return Math.min(n, this.windowStart() + WINDOW_SIZE);
  });

  protected readonly visibleRows = computed<ViewRow[]>(() =>
    toViewRows(this.filteredTurns(), this.windowStart(), this.windowEnd()),
  );

  protected readonly visibleGroups = computed<WorkGroup[]>(() =>
    groupAssistantWork(this.visibleRows()),
  );

  protected readonly earlierCount = computed(() => this.windowStart());
  protected readonly laterCount = computed(
    () => this.filteredTurns().length - this.windowEnd(),
  );

  protected readonly matchTicks = computed(() => {
    const n = this.filteredTurns().length || 1;
    return this.matches().map((m) => ({
      index: m.turnIndex,
      pct: (m.turnIndex / n) * 100,
      current: this.activeMatch()?.turnIndex === m.turnIndex,
    }));
  });

  protected readonly windowThumb = computed(() => {
    const n = this.filteredTurns().length || 1;
    const start = this.windowStart();
    const end = this.windowEnd();
    return {
      top: (start / n) * 100,
      height: Math.max(2.4, ((end - start) / n) * 100),
    };
  });

  protected readonly foldAt = TEXT_FOLD;
  protected readonly minQuery = MIN_QUERY;

  constructor() {
    effect(() => {
      const key = `${this.session().identity.nativeId}:${this.showMeta()}`;
      const n = this.filteredTurns().length;
      untracked(() => {
        void key;
        const w = tailWindow(n);
        this.windowStart.set(w.start);
        this.matchIndex.set(0);
        this.query.set('');
        this.expanded.set(new Set());
        this.textOpen.set(new Set());
        this.openGroups.set(new Set());
      });
    });
  }

  @HostListener('document:keydown', ['$event'])
  onKey(ev: KeyboardEvent): void {
    const t = ev.target as HTMLElement | null;
    const typing =
      t instanceof HTMLInputElement ||
      t instanceof HTMLTextAreaElement ||
      t?.isContentEditable;
    const mod = ev.ctrlKey || ev.metaKey;

    if (mod && (ev.key === 'f' || ev.key === 'F')) {
      ev.preventDefault();
      this.focusSearch();
      return;
    }
    if (!typing && ev.key === '/') {
      ev.preventDefault();
      this.focusSearch();
      return;
    }
    if (ev.key === 'Escape') {
      if (this.query()) {
        ev.preventDefault();
        this.query.set('');
        return;
      }
      ev.preventDefault();
      this.closed.emit();
      return;
    }
    if (ev.key === 'Enter' && this.matches().length) {
      if (t !== this.searchInput()?.nativeElement && !mod) return;
      ev.preventDefault();
      if (ev.shiftKey) this.prevMatch();
      else this.nextMatch();
    }
  }

  protected onQuery(value: string): void {
    this.query.set(value);
    this.matchIndex.set(0);
    const first = findMatches(this.filteredTurns(), value, this.haystacks())[0];
    if (first) this.jumpTo(first.turnIndex);
  }

  protected nextMatch(): void {
    const hits = this.matches();
    if (!hits.length) return;
    const i = (this.matchIndex() + 1) % hits.length;
    this.matchIndex.set(i);
    this.jumpTo(hits[i].turnIndex);
  }

  protected prevMatch(): void {
    const hits = this.matches();
    if (!hits.length) return;
    const i = (this.matchIndex() - 1 + hits.length) % hits.length;
    this.matchIndex.set(i);
    this.jumpTo(hits[i].turnIndex);
  }

  protected loadEarlier(): void {
    this.windowStart.update((s) => Math.max(0, s - WINDOW_SIZE));
  }

  protected loadLater(): void {
    const n = this.filteredTurns().length;
    this.windowStart.update((s) => Math.min(Math.max(0, n - WINDOW_SIZE), s + WINDOW_SIZE));
  }

  protected jumpLatest(): void {
    const w = tailWindow(this.filteredTurns().length);
    this.windowStart.set(w.start);
    this.scrollToTop();
  }

  protected jumpStart(): void {
    this.windowStart.set(0);
    this.scrollToTop();
  }

  protected onRailClick(ev: MouseEvent): void {
    const el = ev.currentTarget as HTMLElement;
    const rect = el.getBoundingClientRect();
    const y = Math.min(1, Math.max(0, (ev.clientY - rect.top) / rect.height));
    const n = this.filteredTurns().length;
    this.jumpTo(Math.min(n - 1, Math.floor(y * n)));
  }

  protected jumpTo(index: number): void {
    const n = this.filteredTurns().length;
    if (n <= 0) return;
    const clamped = Math.min(n - 1, Math.max(0, index));
    const w = windowAround(clamped, n);
    this.windowStart.set(w.start);
    const id = this.filteredTurns()[clamped]?.id;
    if (id) {
      const group = this.visibleGroups().find((g) => g.rows.some((r) => r.turn.id === id));
      if (group && group.kind === 'assistant' && !this.openGroups().has(group.id)) {
        const next = new Set(this.openGroups());
        next.add(group.id);
        this.openGroups.set(next);
      }
      this.scrollToTurn(id);
    }
  }

  protected isGroupOpen(group: WorkGroup): boolean {
    if (group.kind === 'user') return true;
    if (this.mode() === 'full') return true;
    return this.openGroups().has(group.id);
  }

  protected setMode(mode: PreviewMode): void {
    this.mode.set(mode);
    if (mode !== 'full') this.openGroups.set(new Set());
  }

  protected toggleGroup(id: string): void {
    if (this.mode() === 'full') {
      this.mode.set('chat');
      this.openGroups.set(new Set());
      return;
    }
    const next = new Set(this.openGroups());
    if (next.has(id)) next.delete(id);
    else next.add(id);
    this.openGroups.set(next);
  }

  protected rowsForGroup(group: WorkGroup): ViewRow[] {
    if (this.isGroupOpen(group)) return group.rows;
    if (this.mode() === 'work') return group.proseRows;
    return lastProseRows(group);
  }

  protected showBlock(block: Block, groupOpen: boolean): boolean {
    if (groupOpen) return true;
    if (isWorkBlock(block.kind)) return false;
    if (block.kind === 'meta') return this.showMeta();
    return true;
  }

  protected hasHiddenWork(group: WorkGroup): boolean {
    if (group.kind !== 'assistant') return false;
    if (this.isGroupOpen(group)) return !!group.summary;
    if (this.mode() === 'chat') {
      return !!group.summary || group.proseRows.length > 1;
    }
    return !!group.summary;
  }

  protected workBarLabel(group: WorkGroup): string {
    if (this.isGroupOpen(group)) return 'Hide sequence';
    const parts: string[] = [];
    if (group.summary) parts.push(group.summary);
    if (this.mode() === 'chat' && group.proseRows.length > 1) {
      const n = group.proseRows.length - 1;
      parts.push(n === 1 ? '1 earlier reply' : `${n} earlier replies`);
    }
    return parts.join(' · ') || 'Expand sequence';
  }

  protected isExpanded(key: string): boolean {
    return this.expanded().has(key);
  }

  protected toggleExpanded(key: string): void {
    const next = new Set(this.expanded());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    this.expanded.set(next);
  }

  protected isTextOpen(key: string): boolean {
    return this.textOpen().has(key);
  }

  protected toggleText(key: string): void {
    const next = new Set(this.textOpen());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    this.textOpen.set(next);
  }

  protected segs(text: string): HighlightSeg[] {
    return highlightSegments(text, this.query());
  }

  protected displayText(text: string, key: string): string {
    if (text.length <= TEXT_FOLD || this.isTextOpen(key)) return text;
    return text.slice(0, TEXT_FOLD);
  }

  protected blockKey(turn: Turn, i: number): string {
    return `${turn.id}:${i}`;
  }

  protected toolSummary(block: Block): string {
    if (block.kind !== 'tool_call') return 'tool';
    const hint = toolArgHint(block.arguments);
    return hint ? `${block.name}  ${hint}` : block.name;
  }

  protected resultSummary(block: Block): string {
    if (block.kind !== 'tool_result') return 'result';
    const size = jsonSizeHint(block.output);
    const err = block.is_error ? ' (error)' : '';
    return size ? `result${err} · ${size}` : `result${err}`;
  }

  protected previewJson(block: Block): string {
    if (block.kind === 'tool_call') return jsonPreview(block.arguments);
    if (block.kind === 'tool_result') return jsonPreview(block.output);
    if (block.kind === 'meta') return jsonPreview(block.raw);
    return '';
  }

  protected roleLabel(role: ViewRow['displayRole']): string {
    return role;
  }

  protected absoluteIndex(local: number): number {
    return this.windowStart() + local + 1;
  }

  private focusSearch(): void {
    const el = this.searchInput()?.nativeElement;
    el?.focus();
    el?.select();
  }

  private scrollToTop(): void {
    this.scroller()?.nativeElement.scrollTo({ top: 0 });
  }

  private scrollToTurn(id: string): void {
    afterNextRender(
      () => {
        const root = this.scroller()?.nativeElement;
        const node = root?.querySelector(`[data-turn-id="${cssEscape(id)}"]`);
        node?.scrollIntoView({ block: 'center', behavior: 'instant' });
      },
      { injector: this.injector },
    );
  }
}

function cssEscape(id: string): string {
  if (typeof CSS !== 'undefined' && typeof CSS.escape === 'function') {
    return CSS.escape(id);
  }
  return id.replace(/"/g, '\\"');
}
