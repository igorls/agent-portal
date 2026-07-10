import { Component, inject, signal } from '@angular/core';
import { RouterOutlet, RouterLink, RouterLinkActive } from '@angular/router';
import { PortalCommands } from './core/ipc/commands';
import type { Health } from './core/ipc/gen/Health';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  templateUrl: './app.html',
  styleUrl: './app.scss'
})
export class App {
  private readonly commands = inject(PortalCommands);

  protected readonly health = signal<Health | null>(null);
  protected readonly ipcError = signal<string | null>(null);

  constructor() {
    this.commands
      .health()
      .then((h) => this.health.set(h))
      .catch((e) => this.ipcError.set(String(e)));
  }
}
