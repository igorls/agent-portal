import { Routes } from '@angular/router';
import { BoardPage } from './features/board/board-page';
import { ActivityPage } from './features/activity/activity-page';

export const routes: Routes = [
  { path: '', component: BoardPage },
  { path: 'activity', component: ActivityPage },
  {
    path: 'usage',
    title: 'Usage · Agent Portal',
    loadComponent: () => import('./features/usage/usage-page').then((m) => m.UsagePage),
  },
  {
    path: 'settings',
    title: 'Settings · Agent Portal',
    loadComponent: () => import('./features/settings/settings-page').then((m) => m.SettingsPage),
  },
];
