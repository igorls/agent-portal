import {
  afterNextRender,
  Component,
  ElementRef,
  HostListener,
  inject,
  Injector,
  signal,
  viewChild,
} from '@angular/core';
import { Router } from '@angular/router';
import { PortalCommands } from '../../core/ipc/commands';
import { BoardStore } from '../../core/state/board.store';
import type { SearchHit } from '../../core/ipc/gen/SearchHit';

const LANE_ACCENTS: Record<string, string> = {
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

@Component({
  selector: 'app-search-palette',
  templateUrl: './search-palette.html',
  styleUrl: './search-palette.scss',
})
export class SearchPalette {
  private readonly commands = inject(PortalCommands);
  private readonly store = inject(BoardStore);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly searchInput = viewChild<ElementRef<HTMLInputElement>>('searchInput');

  protected readonly open = signal(false);
  protected readonly q = signal('');
  protected readonly hits = signal<SearchHit[]>([]);
  protected readonly loading = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly activeIndex = signal(0);
  protected readonly tookMs = signal<number | null>(null);
  protected readonly indexHint = signal<string | null>(null);
  /** When true and board has an active project, restrict search to that project. */
  protected readonly scopeToProject = signal(false);

  private debounce: ReturnType<typeof setTimeout> | null = null;
  private seq = 0;

  protected boardProject(): { id: string; label: string; path: string | null } | null {
    return this.store.activeProject();
  }

  @HostListener('document:keydown', ['$event'])
  onKey(ev: KeyboardEvent): void {
    const mod = ev.ctrlKey || ev.metaKey;
    if (mod && (ev.key === 'k' || ev.key === 'K')) {
      ev.preventDefault();
      this.toggle();
      return;
    }
    if (!this.open()) return;
    if (ev.key === 'Escape') {
      ev.preventDefault();
      this.close();
      return;
    }
    if (ev.key === 'ArrowDown') {
      ev.preventDefault();
      const n = this.hits().length;
      if (n) this.activeIndex.update((i) => (i + 1) % n);
      return;
    }
    if (ev.key === 'ArrowUp') {
      ev.preventDefault();
      const n = this.hits().length;
      if (n) this.activeIndex.update((i) => (i - 1 + n) % n);
      return;
    }
    if (ev.key === 'Enter') {
      ev.preventDefault();
      const hit = this.hits()[this.activeIndex()];
      if (hit) void this.select(hit);
    }
  }

  protected toggle(): void {
    if (this.open()) this.close();
    else this.show();
  }

  show(): void {
    this.open.set(true);
    this.error.set(null);
    this.q.set('');
    this.hits.set([]);
    this.activeIndex.set(0);
    this.tookMs.set(null);
    // Default to current board project when one is selected.
    this.scopeToProject.set(!!this.store.activeProject());
    void this.refreshIndexHint();
    // Recent sessions on empty query
    void this.runSearch('');
    // Focus after the @if (open) input is in the DOM (Ctrl/⌘K and Search chip).
    afterNextRender(() => this.focusSearchInput(), { injector: this.injector });
  }

  private focusSearchInput(): void {
    const el =
      this.searchInput()?.nativeElement ??
      (document.getElementById('portal-search-input') as HTMLInputElement | null);
    if (!el) {
      // One more frame if paint lagged behind the signal update.
      requestAnimationFrame(() => {
        const retry =
          this.searchInput()?.nativeElement ??
          (document.getElementById('portal-search-input') as HTMLInputElement | null);
        retry?.focus({ preventScroll: true });
      });
      return;
    }
    el.focus({ preventScroll: true });
  }

  protected setProjectScope(scoped: boolean): void {
    if (scoped && !this.store.activeProject()) return;
    if (this.scopeToProject() === scoped) return;
    this.scopeToProject.set(scoped);
    void this.runSearch(this.q());
  }

  private async refreshIndexHint(): Promise<void> {
    try {
      const st = await this.commands.searchIndexStatus();
      if (!st.contentEnabled) {
        this.indexHint.set('content index unavailable');
        return;
      }
      const pct =
        st.boardSessions > 0
          ? Math.min(100, Math.round((st.contentDocs / st.boardSessions) * 100))
          : 0;
      const content =
        st.contentDocs < st.boardSessions
          ? `content ${st.contentDocs}/${st.boardSessions} (${pct}%) indexing…`
          : `content ${st.contentDocs}/${st.boardSessions} indexed`;
      const sem = st.semanticEnabled
        ? st.hnswLive
          ? ' · wormdb up'
          : ' · wormdb ready'
        : '';
      this.indexHint.set(content + sem);
    } catch {
      this.indexHint.set(null);
    }
  }

  protected close(): void {
    this.open.set(false);
    if (this.debounce) clearTimeout(this.debounce);
  }

  protected onInput(value: string): void {
    this.q.set(value);
    if (this.debounce) clearTimeout(this.debounce);
    this.debounce = setTimeout(() => void this.runSearch(value), 120);
  }

  protected accent(agentId: string): string {
    return LANE_ACCENTS[agentId] ?? '#8b93a7';
  }

  protected kindLabel(kind: string): string {
    switch (kind) {
      case 'title':
        return 'title';
      case 'project':
        return 'project';
      case 'path':
        return 'path';
      case 'model':
        return 'model';
      case 'id':
        return 'id';
      case 'fts':
        return 'content';
      case 'semantic':
        return 'semantic';
      case 'recent':
        return 'recent';
      default:
        return kind;
    }
  }

  /** Relative activity time for result rows. */
  protected formatWhen(iso: string | null | undefined): string {
    if (!iso) return '';
    const t = Date.parse(iso);
    if (!Number.isFinite(t)) return '';
    const sec = Math.max(0, Math.round((Date.now() - t) / 1000));
    if (sec < 45) return 'just now';
    if (sec < 3600) return `${Math.round(sec / 60)}m ago`;
    if (sec < 86400) return `${Math.round(sec / 3600)}h ago`;
    if (sec < 86400 * 14) return `${Math.round(sec / 86400)}d ago`;
    if (sec < 86400 * 60) return `${Math.round(sec / (86400 * 7))}w ago`;
    const d = new Date(t);
    return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
  }

  protected formatWhenAbsolute(iso: string | null | undefined): string {
    if (!iso) return '';
    const t = Date.parse(iso);
    if (!Number.isFinite(t)) return iso;
    return new Date(t).toLocaleString();
  }

  protected async select(hit: SearchHit): Promise<void> {
    const projectId = hit.cwdNormalized ?? hit.projectKey;
    this.store.requestJump({
      projectId,
      agentId: hit.agentId,
      nativeId: hit.nativeId,
      storePath: hit.storePath,
      openPreview: true,
    });
    this.close();
    await this.router.navigate(['/']);
  }

  private async runSearch(raw: string): Promise<void> {
    const id = ++this.seq;
    this.loading.set(true);
    this.error.set(null);
    try {
      const proj = this.store.activeProject();
      const scoped = this.scopeToProject() && proj;
      const resp = await this.commands.searchSessions({
        q: raw.trim(),
        limit: 30,
        projectId: scoped ? proj.id : null,
      });
      if (id !== this.seq) return;
      this.hits.set(resp.hits);
      this.tookMs.set(resp.tookMs);
      this.activeIndex.set(0);
    } catch (e) {
      if (id !== this.seq) return;
      this.error.set(String(e));
      this.hits.set([]);
    } finally {
      if (id === this.seq) this.loading.set(false);
    }
  }
}
