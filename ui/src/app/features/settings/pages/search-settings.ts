import { ChangeDetectionStrategy, Component, inject, OnDestroy, OnInit } from '@angular/core';
import { SettingsStore } from '../../../core/state/settings.store';

@Component({
  selector: 'app-search-settings',
  templateUrl: './search-settings.html',
  styleUrl: '../settings-shared.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SearchSettings implements OnInit, OnDestroy {
  protected readonly store = inject(SettingsStore);
  private poll: ReturnType<typeof setInterval> | null = null;

  ngOnInit(): void {
    // Live progress while this tab is open (reindex can take a while with Ollama).
    this.poll = setInterval(() => void this.store.refreshIndexStatus(), 3000);
  }

  ngOnDestroy(): void {
    if (this.poll) clearInterval(this.poll);
  }

  protected indexPct(done: number, total: number): number {
    if (total <= 0) return 0;
    return Math.min(100, Math.round((done / total) * 100));
  }

  /** Human ETA from backend seconds (e.g. "~12 min", "~1h 05m"). */
  protected formatEta(secs: number | null | undefined): string | null {
    if (secs == null || !Number.isFinite(secs) || secs < 0) return null;
    const s = Math.round(secs);
    if (s < 45) return 'under a minute';
    const mins = Math.round(s / 60);
    if (mins < 60) return `~${mins} min`;
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    if (m === 0) return `~${h}h`;
    return `~${h}h ${m.toString().padStart(2, '0')}m`;
  }

  protected formatRate(perMin: number | null | undefined): string | null {
    if (perMin == null || !Number.isFinite(perMin) || perMin <= 0) return null;
    if (perMin >= 10) return `~${Math.round(perMin)}/min`;
    return `~${perMin.toFixed(1)}/min`;
  }

  /** Sessions already on the semantic side: board − pending (clamped). */
  protected semanticDone(idx: {
    boardSessions: number;
    semanticPending?: number;
  }): number {
    const pending = idx.semanticPending ?? 0;
    return Math.max(0, idx.boardSessions - pending);
  }

  protected setProvider(value: string): void {
    this.store.patch({ embedProvider: value === 'ollama' ? 'ollama' : 'hash' });
  }

  protected setEmbedModel(value: string): void {
    this.store.patch({ ollamaEmbedModel: value });
  }

  protected save(): void {
    void this.store.save();
  }

  protected refreshIndex(): void {
    void this.store.refreshIndexStatus();
  }

  protected resetSemantic(): void {
    if (
      !confirm(
        'Clear the WormDB semantic index and rebuild with the current embedder?\n\n' +
          'This is required after changing embedding model or dimension. Content (FTS) search is unaffected.',
      )
    ) {
      return;
    }
    void this.store.resetSemanticIndex();
  }

  protected togglePause(): void {
    const paused = !!this.store.indexStatus()?.semanticPaused;
    void this.store.setSemanticPaused(!paused);
  }
}
