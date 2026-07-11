import { Component, inject, signal } from '@angular/core';
import { RouterOutlet, RouterLink, RouterLinkActive } from '@angular/router';
import { PortalCommands } from './core/ipc/commands';
import { TauriService } from './core/ipc/tauri.service';
import { PeekPanel } from './features/peek/peek-panel';
import type { Health } from './core/ipc/gen/Health';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, PeekPanel],
  templateUrl: './app.html',
  styleUrl: './app.scss'
})
export class App {
  private readonly commands = inject(PortalCommands);
  protected readonly tauri = inject(TauriService);

  /** the peek popup runs the same app in a window labelled "peek" */
  protected readonly isPeek = this.tauri.label === 'peek';

  protected readonly health = signal<Health | null>(null);
  protected readonly ipcError = signal<string | null>(null);

  constructor() {
    if (this.isPeek) return;
    this.commands
      .health()
      .then((h) => this.health.set(h))
      .catch((e) => this.ipcError.set(String(e)));
  }
}
