import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import type { LaunchShell } from '../../../core/ipc/gen/LaunchShell';
import { SettingsStore } from '../../../core/state/settings.store';

interface ShellOption {
  id: LaunchShell;
  label: string;
  hint: string;
  platforms: Array<'windows' | 'mac' | 'linux'>;
}

const SHELL_OPTIONS: ShellOption[] = [
  { id: 'auto', label: 'Auto', hint: 'Best available for this OS', platforms: ['windows', 'mac', 'linux'] },
  { id: 'pwsh', label: 'PowerShell 7', hint: 'pwsh', platforms: ['windows', 'mac', 'linux'] },
  {
    id: 'power_shell',
    label: 'Windows PowerShell',
    hint: 'powershell.exe (Windows only)',
    platforms: ['windows'],
  },
  { id: 'cmd', label: 'Command Prompt', hint: 'cmd.exe (Windows only)', platforms: ['windows'] },
  {
    id: 'bash',
    label: 'Bash',
    hint: 'Git Bash / WSL bash on Windows; /bin/bash elsewhere',
    platforms: ['windows', 'mac', 'linux'],
  },
  { id: 'zsh', label: 'zsh', hint: 'macOS / Linux', platforms: ['mac', 'linux'] },
  { id: 'fish', label: 'fish', hint: 'macOS / Linux', platforms: ['mac', 'linux'] },
];

@Component({
  selector: 'app-general-settings',
  templateUrl: './general-settings.html',
  styleUrl: '../settings-shared.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GeneralSettings {
  protected readonly store = inject(SettingsStore);

  protected readonly platform = ((): 'windows' | 'mac' | 'linux' => {
    const ua = navigator.userAgent;
    if (ua.includes('Macintosh')) return 'mac';
    if (ua.includes('Windows')) return 'windows';
    return 'linux';
  })();

  protected readonly shellOptions = SHELL_OPTIONS.filter((o) =>
    o.platforms.includes(this.platform),
  );

  protected onShellChange(value: string): void {
    this.store.patch({ launchShell: value as LaunchShell });
  }

  protected save(): void {
    void this.store.save();
  }
}
