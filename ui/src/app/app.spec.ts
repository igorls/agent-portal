import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { App } from './app';
import { TauriService } from './core/ipc/tauri.service';

const tauri = {
  label: 'main',
  maximized: signal(false),
  invoke: async () => ({
    appVersion: '0.1.1',
    adaptersRegistered: 6,
  }),
  hideToTray: async () => undefined,
  toggleMaximizeWindow: async () => undefined,
};

describe('App', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [App],
      providers: [provideRouter([]), { provide: TauriService, useValue: tauri }],
    }).compileComponents();
  });

  it('should create the app', () => {
    const fixture = TestBed.createComponent(App);
    const app = fixture.componentInstance;
    expect(app).toBeTruthy();
  });

  it('should render the main application shell', async () => {
    const fixture = TestBed.createComponent(App);
    fixture.detectChanges();
    await fixture.whenStable();
    const compiled = fixture.nativeElement as HTMLElement;
    expect(compiled.querySelector('.name')?.textContent).toContain('Agent Portal');
    expect(compiled.querySelector('nav')?.textContent).toContain('Board');
  });
});
