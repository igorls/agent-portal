import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormField, form, pattern, required, submit } from '@angular/forms/signals';
import { PortalCommands } from '../../core/ipc/commands';
import type { OllamaStatus } from '../../core/ipc/gen/OllamaStatus';

@Component({
  selector: 'app-settings-page',
  imports: [FormField],
  templateUrl: './settings-page.html',
  styleUrl: './settings-page.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  private readonly commands = inject(PortalCommands);
  protected readonly model = signal({ ollamaHost: '', ollamaModel: '' });
  protected readonly settingsForm = form(this.model, (s) => {
    required(s.ollamaHost, { message: 'Host is required' });
    pattern(s.ollamaHost, /^https?:\/\/.+/, { message: 'Use an http:// or https:// URL' });
    required(s.ollamaModel, { message: 'Model is required' });
  });
  protected readonly loading = signal(true);
  protected readonly saving = signal(false);
  protected readonly saved = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly ollama = signal<OllamaStatus | null>(null);

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
}
