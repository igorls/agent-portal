import { Injectable, inject, signal } from '@angular/core';
import { PortalCommands } from '../ipc/commands';
import type { AppSettings } from '../ipc/gen/AppSettings';
import type { LaunchShell } from '../ipc/gen/LaunchShell';
import type { OllamaStatus } from '../ipc/gen/OllamaStatus';
import type { ModelRecommendation } from '../ipc/gen/ModelRecommendation';

const DEFAULTS: AppSettings = {
  ollamaHost: 'http://localhost:11434',
  ollamaNamingModel: '',
  ollamaModel: '',
  launchShell: 'auto',
  mcpHttpEnabled: true,
  mcpHttpPort: 17432,
  mcpHttpToken: '',
  embedProvider: 'hash',
  ollamaEmbedModel: '',
};

@Injectable({ providedIn: 'root' })
export class SettingsStore {
  private readonly commands = inject(PortalCommands);

  readonly settings = signal<AppSettings>({ ...DEFAULTS });
  /** Last successfully saved settings (for embedder-change detection). */
  readonly savedSettings = signal<AppSettings>({ ...DEFAULTS });
  readonly loading = signal(true);
  readonly saving = signal(false);
  readonly saved = signal(false);
  readonly error = signal<string | null>(null);
  readonly ollama = signal<OllamaStatus | null>(null);
  readonly pulling = signal<string | null>(null);
  readonly refreshing = signal(false);
  readonly mcpStatus = signal<{
    enabled: boolean;
    running: boolean;
    port: number | null;
    url: string | null;
    tokenRequired: boolean;
  } | null>(null);
  readonly indexStatus = signal<{
    contentDocs: number;
    boardSessions: number;
    contentEnabled: boolean;
    semanticEnabled: boolean;
    semanticChunks: number;
    hnswLive: boolean;
    semanticActive?: boolean;
    semanticCurrent?: string | null;
    semanticPending?: number;
    semanticError?: string | null;
    semanticEmbedder?: string | null;
    semanticPaused?: boolean;
    semanticEtaSecs?: number | null;
    semanticRatePerMin?: number | null;
    semanticDoneWindow?: number;
  } | null>(null);
  readonly indexRefreshing = signal(false);
  readonly reindexing = signal(false);
  readonly pauseToggling = signal(false);

  async load(): Promise<void> {
    // Only block the shell on the tiny settings.json read. Ollama / index /
    // MCP probes can take seconds (offline Ollama, WormDB STATUS) and must
    // not freeze navigation into Settings.
    this.loading.set(true);
    this.error.set(null);
    try {
      const s = await this.commands.getSettings();
      const normalized = this.normalize(s);
      this.settings.set(normalized);
      this.savedSettings.set({ ...normalized });
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
    // Fire secondary probes in parallel; each page shows what it needs.
    void this.refreshSecondary();
  }

  /** Non-blocking status probes used by Models / Search / MCP tabs. */
  async refreshSecondary(): Promise<void> {
    await Promise.all([
      this.refreshOllama().catch(() => undefined),
      this.commands
        .mcpHttpStatus()
        .then((s) => this.mcpStatus.set(s))
        .catch(() => this.mcpStatus.set(null)),
      this.refreshIndexStatus().catch(() => undefined),
    ]);
  }

  patch(partial: Partial<AppSettings>): void {
    this.settings.update((s) => ({ ...s, ...partial }));
    this.saved.set(false);
  }

  /** True when embed provider/model differs from last saved — save will reindex. */
  embedderDirty(): boolean {
    const a = this.settings();
    const b = this.savedSettings();
    return (
      a.embedProvider !== b.embedProvider ||
      a.ollamaEmbedModel.trim() !== b.ollamaEmbedModel.trim()
    );
  }

  async save(): Promise<boolean> {
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(false);
    const willReindex = this.embedderDirty();
    try {
      const payload = this.normalize(this.settings());
      const saved = this.normalize(await this.commands.saveSettings(payload));
      this.settings.set(saved);
      this.savedSettings.set({ ...saved });
      this.ollama.set(await this.commands.checkOllama());
      this.mcpStatus.set(await this.commands.mcpHttpStatus().catch(() => null));
      this.indexStatus.set(await this.commands.searchIndexStatus().catch(() => null));
      this.saved.set(true);
      if (willReindex) {
        // Server already cleared; refresh status after a beat.
        setTimeout(() => void this.refreshIndexStatus(), 1500);
      }
      return true;
    } catch (e) {
      this.error.set(String(e));
      return false;
    } finally {
      this.saving.set(false);
    }
  }

  async refreshOllama(): Promise<void> {
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

  async refreshIndexStatus(): Promise<void> {
    this.indexRefreshing.set(true);
    try {
      this.indexStatus.set(await this.commands.searchIndexStatus());
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.indexRefreshing.set(false);
    }
  }

  async resetSemanticIndex(): Promise<void> {
    this.reindexing.set(true);
    this.error.set(null);
    try {
      await this.commands.resetSemanticIndex();
      await this.refreshIndexStatus();
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.reindexing.set(false);
    }
  }

  async setSemanticPaused(paused: boolean): Promise<void> {
    this.pauseToggling.set(true);
    this.error.set(null);
    try {
      await this.commands.setSemanticIndexingPaused(paused);
      await this.refreshIndexStatus();
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.pauseToggling.set(false);
    }
  }

  async pullModel(choice: ModelRecommendation): Promise<void> {
    this.pulling.set(choice.name);
    this.error.set(null);
    try {
      this.ollama.set(await this.commands.pullOllamaModel(choice.name));
      this.patch({ ollamaNamingModel: choice.name });
    } catch (e) {
      this.error.set(String(e));
    } finally {
      this.pulling.set(null);
    }
  }

  private normalize(s: AppSettings): AppSettings {
    return {
      ollamaHost: s.ollamaHost ?? DEFAULTS.ollamaHost,
      ollamaNamingModel: s.ollamaNamingModel ?? '',
      ollamaModel: s.ollamaModel ?? '',
      launchShell: (s.launchShell as LaunchShell) ?? 'auto',
      mcpHttpEnabled: s.mcpHttpEnabled ?? true,
      mcpHttpPort: s.mcpHttpPort ?? 17432,
      mcpHttpToken: s.mcpHttpToken ?? '',
      embedProvider: s.embedProvider === 'ollama' ? 'ollama' : 'hash',
      ollamaEmbedModel: s.ollamaEmbedModel ?? '',
    };
  }
}
