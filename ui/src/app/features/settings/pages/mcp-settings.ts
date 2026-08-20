import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { SettingsStore } from '../../../core/state/settings.store';

@Component({
  selector: 'app-mcp-settings',
  templateUrl: './mcp-settings.html',
  styleUrl: '../settings-shared.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class McpSettings {
  protected readonly store = inject(SettingsStore);

  protected setEnabled(checked: boolean): void {
    this.store.patch({ mcpHttpEnabled: checked });
  }

  protected setPort(value: string): void {
    const n = Number(value);
    this.store.patch({ mcpHttpPort: Number.isFinite(n) ? n : 17432 });
  }

  protected setToken(value: string): void {
    this.store.patch({ mcpHttpToken: value });
  }

  protected save(): void {
    void this.store.save();
  }
}
