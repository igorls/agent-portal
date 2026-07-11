import { Injectable, signal } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * The only file in the app that touches @tauri-apps/api.
 * Everything else goes through typed wrappers in commands.ts / events.ts.
 */
@Injectable({ providedIn: 'root' })
export class TauriService {
  private readonly win = getCurrentWindow();
  /** which window this Angular instance is running in ("main" or "peek") */
  readonly label = this.win.label;
  /** whether the window is currently maximized (drives the restore/maximize icon) */
  readonly maximized = signal(false);

  constructor() {
    void this.syncMaximized();
    void this.win.onResized(() => this.syncMaximized());
  }

  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(cmd, args);
  }

  /** Tuck the window away to the system tray (keeps the app running). */
  hideToTray(): Promise<void> {
    return this.win.hide();
  }

  toggleMaximizeWindow(): Promise<void> {
    return this.win.toggleMaximize();
  }

  /** Restore the full board window (used from the peek popup / tray). */
  showMainBoard(): Promise<void> {
    return this.invoke<void>('show_main_window');
  }

  closePeek(): Promise<void> {
    return this.win.hide();
  }

  private async syncMaximized(): Promise<void> {
    try {
      this.maximized.set(await this.win.isMaximized());
    } catch {
      /* running outside Tauri (tests) */
    }
  }
}
