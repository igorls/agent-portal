import { Injectable, computed, inject, signal } from '@angular/core';
import { PortalCommands } from '../ipc/commands';
import type { BoardSnapshot } from '../ipc/gen/BoardSnapshot';

@Injectable({ providedIn: 'root' })
export class BoardStore {
  private readonly commands = inject(PortalCommands);

  readonly board = signal<BoardSnapshot | null>(null);
  /** true while a full store scan is in flight (cached data may be showing) */
  readonly refreshing = signal(false);
  readonly error = signal<string | null>(null);
  /** true only before the very first data (cached or fresh) arrives */
  readonly coldLoading = signal(false);

  readonly totalSessions = computed(() => {
    const board = this.board();
    if (!board) return 0;
    return board.lanes.reduce(
      (acc, lane) => acc + lane.projects.reduce((a, p) => a + p.sessions.length, 0),
      0
    );
  });

  /** Show the cached board instantly, then refresh from a full scan. */
  async load(): Promise<void> {
    this.error.set(null);
    if (!this.board()) {
      this.coldLoading.set(true);
      try {
        const cached = await this.commands.getCachedBoard();
        if (cached) {
          this.board.set(cached);
          this.coldLoading.set(false);
        }
      } catch {
        /* cache miss is fine; the refresh below fills it */
      }
    }
    await this.refresh();
  }

  /** Force a full re-scan (also updates the on-disk cache). */
  async refresh(): Promise<void> {
    this.refreshing.set(true);
    try {
      this.board.set(await this.commands.refreshBoard());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.refreshing.set(false);
      this.coldLoading.set(false);
    }
  }
}
