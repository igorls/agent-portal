import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import type { ModelRecommendation } from '../../../core/ipc/gen/ModelRecommendation';
import { SettingsStore } from '../../../core/state/settings.store';

@Component({
  selector: 'app-models-settings',
  templateUrl: './models-settings.html',
  styleUrl: '../settings-shared.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ModelsSettings {
  protected readonly store = inject(SettingsStore);

  protected onHostInput(value: string): void {
    this.store.patch({ ollamaHost: value });
  }

  protected onNamingInput(value: string): void {
    this.store.patch({ ollamaNamingModel: value });
  }

  protected onHandoffInput(value: string): void {
    this.store.patch({ ollamaModel: value });
  }

  protected selectNaming(model: string): void {
    this.store.patch({ ollamaNamingModel: model });
  }

  protected pull(choice: ModelRecommendation): void {
    void this.store.pullModel(choice);
  }

  protected refresh(): void {
    void this.store.refreshOllama();
  }

  protected save(): void {
    void this.store.save();
  }

  protected formatBytes(value: number | bigint): string {
    const bytes = Number(value);
    if (!bytes) return 'Unknown size';
    const gib = bytes / 1024 ** 3;
    return gib >= 1 ? `${gib.toFixed(1)} GB` : `${Math.round(bytes / 1024 ** 2)} MB`;
  }
}
