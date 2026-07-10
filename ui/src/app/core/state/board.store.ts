import { Injectable, computed, inject, signal } from '@angular/core';
import { PortalCommands } from '../ipc/commands';
import type { BoardSnapshot } from '../ipc/gen/BoardSnapshot';

@Injectable({ providedIn: 'root' })
export class BoardStore {
  private readonly commands = inject(PortalCommands);

  readonly board = signal<BoardSnapshot | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  readonly totalSessions = computed(() => {
    const board = this.board();
    if (!board) return 0;
    return board.lanes.reduce(
      (acc, lane) => acc + lane.projects.reduce((a, p) => a + p.sessions.length, 0),
      0
    );
  });

  async load(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      this.board.set(await this.commands.getBoard());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }
}
