import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormField, form, pattern, required, submit } from '@angular/forms/signals';
import { PortalCommands } from '../../core/ipc/commands';
import type { AppSettings } from '../../core/ipc/gen/AppSettings';
import type { LaunchShell } from '../../core/ipc/gen/LaunchShell';
import type { OllamaStatus } from '../../core/ipc/gen/OllamaStatus';
import type { ModelRecommendation } from '../../core/ipc/gen/ModelRecommendation';

interface ShellOption {
  id: LaunchShell;
  label: string;
  hint: string;
  /** Show on Windows / macOS / Linux (userAgent heuristics). */
  platforms: Array<'windows' | 'mac' | 'linux'>;
}

const SHELL_OPTIONS: ShellOption[] = [
  {
    id: 'auto',
    label: 'Auto',
    hint: 'Best available for this OS',
    platforms: ['windows', 'mac', 'linux'],
  },
  {
    id: 'pwsh',
    label: 'PowerShell 7',
    hint: 'pwsh',
    platforms: ['windows', 'mac', 'linux'],
  },
  {
    id: 'power_shell',
    label: 'Windows PowerShell',
    hint: 'powershell.exe (Windows only)',
    platforms: ['windows'],
  },
  {
    id: 'cmd',
    label: 'Command Prompt',
    hint: 'cmd.exe (Windows only)',
    platforms: ['windows'],
  },
  {
    id: 'bash',
    label: 'Bash',
    hint: 'Git Bash / WSL bash on Windows; /bin/bash elsewhere',
    platforms: ['windows', 'mac', 'linux'],
  },
  {
    id: 'zsh',
    label: 'zsh',
    hint: 'macOS / Linux',
    platforms: ['mac', 'linux'],
  },
  {
    id: 'fish',
    label: 'fish',
    hint: 'macOS / Linux',
    platforms: ['mac', 'linux'],
  },
];

@Component({
  selector: 'app-settings-page',
  imports: [FormField],
  templateUrl: './settings-page.html',
  styleUrl: './settings-page.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  private readonly commands = inject(PortalCommands);
  protected readonly model = signal<AppSettings>({
    ollamaHost: '',
    ollamaNamingModel: '',
    ollamaModel: '',
    launchShell: 'auto',
  });
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

  protected readonly platform = ((): 'windows' | 'mac' | 'linux' => {
    const ua = navigator.userAgent;
    if (ua.includes('Macintosh')) return 'mac';
    if (ua.includes('Windows')) return 'windows';
    return 'linux';
  })();

  protected readonly shellOptions = SHELL_OPTIONS.filter((o) =>
    o.platforms.includes(this.platform)
  );

  constructor() {
    void this.load();
  }

  private async load(): Promise<void> {
    try {
      const settings = await this.commands.getSettings();
      // Always normalize so a missing/legacy field still selects a valid option.
      this.model.set({
        ollamaHost: settings.ollamaHost,
        ollamaNamingModel: settings.ollamaNamingModel,
        ollamaModel: settings.ollamaModel,
        launchShell: this.normalizeShell(settings.launchShell),
      });
      this.ollama.set(await this.commands.checkOllama());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  protected onLaunchShellChanged(): void {
    this.saved.set(false);
  }

  protected save(): void {
    submit(this.settingsForm, async () => {
      this.saving.set(true);
      this.error.set(null);
      this.saved.set(false);
      try {
        // Read from the form-backed model so launchShell is included even if
        // the user only touched the shell select.
        const payload: AppSettings = {
          ...this.model(),
          launchShell: this.normalizeShell(this.model().launchShell),
        };
        const saved = await this.commands.saveSettings(payload);
        this.model.set({
          ...saved,
          launchShell: this.normalizeShell(saved.launchShell),
        });
        this.ollama.set(await this.commands.checkOllama());
        this.saved.set(true);
      } catch (e) {
        this.error.set(String(e));
      } finally {
        this.saving.set(false);
      }
    });
  }

  private normalizeShell(value: LaunchShell | string | null | undefined): LaunchShell {
    const allowed = new Set(this.shellOptions.map((o) => o.id));
    if (value && allowed.has(value as LaunchShell)) return value as LaunchShell;
    // Fall back to Auto when the stored value isn't offered on this OS.
    return 'auto';
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
