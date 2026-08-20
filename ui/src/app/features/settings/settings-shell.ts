import { ChangeDetectionStrategy, Component, inject, OnInit } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { SettingsStore } from '../../core/state/settings.store';

@Component({
  selector: 'app-settings-shell',
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  templateUrl: './settings-shell.html',
  styleUrl: './settings-shell.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsShell implements OnInit {
  protected readonly store = inject(SettingsStore);

  ngOnInit(): void {
    void this.store.load();
  }
}
