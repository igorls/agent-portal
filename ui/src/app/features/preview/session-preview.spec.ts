import { TestBed } from '@angular/core/testing';
import { vi } from 'vitest';
import { PortalCommands } from '../../core/ipc/commands';
import type { CanonicalSession } from '../../core/ipc/gen/CanonicalSession';
import { SessionPreview } from './session-preview';

describe('SessionPreview', () => {
  it('opens the session with its owning agent', async () => {
    const launchSession = vi.fn().mockResolvedValue(undefined);
    await TestBed.configureTestingModule({
      imports: [SessionPreview],
      providers: [{ provide: PortalCommands, useValue: { launchSession } }],
    }).compileComponents();

    const session: CanonicalSession = {
      irVersion: 1,
      identity: {
        portalId: 'portal-id',
        nativeId: 'grok-session-id',
        agentId: 'grok-build',
        storePath: 'C:\\Users\\test\\.grok\\sessions\\grok-session-id',
        agentVersion: '1.0.5',
        readAt: '2026-08-20T00:00:00Z',
      },
      workspace: {
        cwd: 'P:\\project',
        cwdNormalized: 'p:/project',
        gitBranch: 'main',
        projectLabel: 'project',
      },
      title: 'Migrated session',
      timeline: [],
      attachments: [],
      usage: { inputTokens: 0, outputTokens: 0, known: false },
      losses: [],
      fidelity: 'partial',
    };
    const fixture = TestBed.createComponent(SessionPreview);
    fixture.componentRef.setInput('session', session);
    fixture.detectChanges();

    const button = fixture.nativeElement.querySelector('.open-agent') as HTMLButtonElement;
    expect(button.textContent).toContain('Open in Grok Build');
    button.click();
    await fixture.whenStable();

    expect(launchSession).toHaveBeenCalledWith(
      'grok-build',
      'grok-session-id',
      'P:\\project',
    );
  });
});
