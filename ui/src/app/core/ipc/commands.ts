import { Injectable, inject } from '@angular/core';
import { TauriService } from './tauri.service';
import type { AgentDescriptor } from './gen/AgentDescriptor';
import type { BoardSnapshot } from './gen/BoardSnapshot';
import type { CanonicalSession } from './gen/CanonicalSession';
import type { CommandSpec } from './gen/CommandSpec';
import type { DryRunReport } from './gen/DryRunReport';
import type { Health } from './gen/Health';
import type { LedgerEntry } from './gen/LedgerEntry';
import type { MigrationResult } from './gen/MigrationResult';
import type { OllamaStatus } from './gen/OllamaStatus';
import type { UndoReport } from './gen/UndoReport';

export type MigrationMode = 'native' | 'brief';

/**
 * Typed wrappers for every Tauri command. Command names here must stay in
 * sync with src-tauri/src/commands.rs (CI compares the two lists).
 */
@Injectable({ providedIn: 'root' })
export class PortalCommands {
  private readonly tauri = inject(TauriService);

  health(): Promise<Health> {
    return this.tauri.invoke<Health>('health');
  }

  detectAgents(): Promise<AgentDescriptor[]> {
    return this.tauri.invoke<AgentDescriptor[]>('detect_agents');
  }

  getBoard(): Promise<BoardSnapshot> {
    return this.tauri.invoke<BoardSnapshot>('get_board');
  }

  getSessionPreview(
    agentId: string,
    nativeId: string,
    storePath?: string
  ): Promise<CanonicalSession> {
    return this.tauri.invoke<CanonicalSession>('get_session_preview', {
      agentId,
      nativeId,
      storePath: storePath ?? null,
    });
  }

  checkOllama(): Promise<OllamaStatus> {
    return this.tauri.invoke<OllamaStatus>('check_ollama');
  }

  planMigration(
    sourceAgent: string,
    sourceNativeId: string,
    targetAgent: string,
    mode: MigrationMode,
    opts: { enhance?: boolean; sourceStorePath?: string } = {}
  ): Promise<DryRunReport> {
    return this.tauri.invoke<DryRunReport>('plan_migration', {
      sourceAgent,
      sourceNativeId,
      sourceStorePath: opts.sourceStorePath ?? null,
      targetAgent,
      mode,
      enhance: opts.enhance ?? false,
    });
  }

  executeMigration(planId: string): Promise<MigrationResult> {
    return this.tauri.invoke<MigrationResult>('execute_migration', { planId });
  }

  launchSession(agentId: string, nativeId: string, cwd: string): Promise<void> {
    return this.tauri.invoke<void>('launch_session', { agentId, nativeId, cwd });
  }

  launchCommand(spec: CommandSpec): Promise<void> {
    return this.tauri.invoke<void>('launch_command', { spec });
  }

  listActivity(): Promise<LedgerEntry[]> {
    return this.tauri.invoke<LedgerEntry[]>('list_activity');
  }

  undoMigration(migrationId: string, force = false): Promise<UndoReport> {
    return this.tauri.invoke<UndoReport>('undo_migration', { migrationId, force });
  }
}
