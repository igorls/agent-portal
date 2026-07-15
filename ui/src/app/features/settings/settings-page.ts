import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormField, form, pattern, required, submit } from '@angular/forms/signals';
import { PortalCommands } from '../../core/ipc/commands';
import type { OllamaStatus } from '../../core/ipc/gen/OllamaStatus';
import type { ModelRecommendation } from '../../core/ipc/gen/ModelRecommendation';

@Component({
  selector: 'app-settings-page',
  imports: [FormField],
  templateUrl: './settings-page.html',
  styleUrl: './settings-page.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  private readonly commands = inject(PortalCommands);
  protected readonly model = signal({ ollamaHost: '', ollamaNamingModel: '', ollamaModel: '' });
  protected readonly settingsForm = form(this.model, (s) => {
    required(s.ollamaHost, { message: 'Host is required' });
    pattern(s.ollamaHost, /^https?:\/\/.+/, { message: 'Use an http:// or https:// URL' });
    required(s.ollamaModel, { message: 'Model is required' });
    required(s.ollamaNamingModel, { message: 'Naming model is required' });
  });
  protected readonly loading = signal(true);
  protected readonly saving = signal(false);
  protected readonly saved = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly ollama = signal<OllamaStatus | null>(null);
  protected readonly pulling = signal<string | null>(null);
  protected readonly refreshing = signal(false);

  constructor() {
    void this.load();
  }

  private async load(): Promise<void> {
    try {
      this.model.set(await this.commands.getSettings());
      this.ollama.set(await this.commands.checkOllama());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  protected save(): void {
    submit(this.settingsForm, async () => {
      this.saving.set(true);
      this.error.set(null);
      this.saved.set(false);
      try {
        this.model.set(await this.commands.saveSettings(this.model()));
        this.ollama.set(await this.commands.checkOllama());
        this.saved.set(true);
      } catch (e) {
        this.error.set(String(e));
      } finally {
        this.saving.set(false);
      }
    });
  }

  protected selectModel(model: string): void {
    this.model.update((settings) => ({ ...settings, ollamaNamingModel: model }));
    this.saved.set(false);
  }

  protected async refresh(): Promise<void> {
    this.refreshing.set(true);
    this.error.set(null);
    try {
      this.ollama.set(await this.commands.checkOllama());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.refreshing.set(false);
    }
  }

  protected async pull(choice: ModelRecommendation): Promise<void> {
    this.pulling.set(choice.name);
    this.error.set(null);
    try {
      this.ollama.set(await this.commands.pullOllamaModel(choice.name));
      this.selectModel(choice.name);
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.pulling.set(null);
    }
  }

  protected formatBytes(value: number | bigint): string {
    const bytes = Number(value);
    if (!bytes) return 'Unknown size';
    const gib = bytes / 1024 ** 3;
    return gib >= 1 ? `${gib.toFixed(1)} GB` : `${Math.round(bytes / 1024 ** 2)} MB`;
  }
}
