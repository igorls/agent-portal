import { Component, computed, input, output, signal } from '@angular/core';
import { DatePipe, JsonPipe } from '@angular/common';
import type { CanonicalSession } from '../../core/ipc/gen/CanonicalSession';
import type { Turn } from '../../core/ipc/gen/Turn';

const TURN_CAP = 300;

/**
 * Read-only transcript renderer over the canonical IR. The same component
 * verifies migrations later (pre/post views render through identical code).
 */
@Component({
  selector: 'app-session-preview',
  imports: [DatePipe, JsonPipe],
  templateUrl: './session-preview.html',
  styleUrl: './session-preview.scss',
})
export class SessionPreview {
  readonly session = input.required<CanonicalSession>();
  readonly closed = output<void>();

  protected readonly showMeta = signal(false);
  protected readonly showAll = signal(false);

  protected readonly visibleTurns = computed<Turn[]>(() => {
    const turns = this.session().timeline.filter((t) => this.showMeta() || !t.isMeta);
    return this.showAll() ? turns : turns.slice(0, TURN_CAP);
  });

  protected readonly hiddenCount = computed(() => {
    const total = this.session().timeline.filter((t) => this.showMeta() || !t.isMeta).length;
    return Math.max(0, total - this.visibleTurns().length);
  });

  protected asAny(value: unknown): any {
    return value;
  }

  protected roleLabel(turn: Turn): string {
    switch (turn.role) {
      case 'user':
        return 'user';
      case 'assistant':
        return 'assistant';
      case 'tool':
        return 'tool';
      default:
        return 'system';
    }
  }
}
