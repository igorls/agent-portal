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
    loadComponent: () =>
      import('./features/settings/settings-shell').then((m) => m.SettingsShell),
    children: [
      { path: '', pathMatch: 'full', redirectTo: 'general' },
      {
        path: 'general',
        loadComponent: () =>
          import('./features/settings/pages/general-settings').then((m) => m.GeneralSettings),
      },
      {
        path: 'models',
        loadComponent: () =>
          import('./features/settings/pages/models-settings').then((m) => m.ModelsSettings),
      },
      {
        path: 'search',
        loadComponent: () =>
          import('./features/settings/pages/search-settings').then((m) => m.SearchSettings),
      },
      {
        path: 'mcp',
        loadComponent: () =>
          import('./features/settings/pages/mcp-settings').then((m) => m.McpSettings),
      },
    ],
  },
];
