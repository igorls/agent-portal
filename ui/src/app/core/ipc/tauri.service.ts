import { Injectable } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';

/**
 * The only file in the app that touches @tauri-apps/api.
 * Everything else goes through typed wrappers in commands.ts / events.ts.
 */
@Injectable({ providedIn: 'root' })
export class TauriService {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(cmd, args);
  }
}
