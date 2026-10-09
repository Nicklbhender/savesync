// Stand-in for the Tauri backend so the UI can be previewed in a normal browser:
//   python3 -m http.server -d desktop/ui 8765   →  http://localhost:8765/?mock=paired
// Does nothing inside the app, where window.__TAURI__ is provided.
(() => {
  if (window.__TAURI__) return;

  const now = Date.now();
  const min = 60 * 1000;
  const version = (game_id, version, device_name, playedAgo) => ({
    game_id, version, base_version: version - 1, device_id: 'dev_x', device_name,
    created_at: now - playedAgo + 2 * min, played_at: now - playedAgo, total_size: 131072, file_count: 1, note: null,
  });
  const game = (id, name, location, extra = {}) => ({
    config: { id, name, location, include: [], ignore: [], processes: ['emulator.exe'], import_policy: 'ask' },
    base_version: 4, registered: true, in_session: false, queued_upload: null, staged: null, conflict: null, last_error: null,
    ...extra,
  });

  const params = new URLSearchParams(location.search);
  const state = {
    server: params.get('mock') === 'setup' ? null : { url: 'http://192.168.1.50:8420', device_id: 'dev_3fa2c1d9e0b7a6f5', device_name: 'Gaming PC' },
    online: params.get('mock') !== 'offline',
    suggested_device_name: 'Gaming PC',
    platform: 'windows',
    autostart: true,
    version: '1.0.0',
    update: { current: '1.0.0', state: 'up_to_date', latest: null, message: null },
    games: params.get('mock') === 'empty' ? [] : [
      game('adventure-game', 'Adventure Game', 'C:\\Games\\Saves', {
        staged: version('adventure-game', 7, 'Handheld', 25 * min),
      }),
      game('puzzle-game', 'Puzzle Game', 'C:\\Games\\Saves', {
        conflict: { remote: version('puzzle-game', 12, 'Laptop', 90 * min), local_played_at: now - 40 * min, detected_at: now },
      }),
      game('platform-game', 'Platform Game', 'C:\\Games\\Saves', { in_session: true }),
      game('strategy-game', 'Strategy Game', 'D:\\Games\\Memory Cards', {
        queued_upload: { played_at: now - 3 * 60 * min, created_at: now - 3 * 60 * min, blocked: false },
      }),
      game('fighting-game', 'Fighting Game', 'C:\\Users\\you\\Documents\\Saves', { base_version: 23 }),
      game('role-playing-game', 'Role-Playing Game', 'E:\\Games\\Saves', { last_error: 'save folder not found: E:\\Games\\Saves' }),
    ],
  };

  const find = (id) => state.games.find((g) => g.config.id === id);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const backups = [
    { id: 3, game_id: 'adventure-game', created_at: now - 2 * 86400000, reason: 'before importing v6', file_count: 1, total_size: 131072 },
    { id: 2, game_id: 'adventure-game', created_at: now - 5 * 86400000, reason: 'before importing v4', file_count: 1, total_size: 131072 },
  ];

  const commands = {
    get_snapshot: () => structuredClone(state),
    pair: async ({ serverUrl, deviceName }) => {
      await sleep(600);
      if (!serverUrl.startsWith('http')) throw 'The server address should start with http://';
      state.server = { url: serverUrl, device_id: 'dev_mock', device_name: deviceName };
      return state.server;
    },
    unpair: () => { state.server = null; },
    set_server_url: ({ url }) => { state.server.url = url; },
    add_game: ({ config }) => {
      if (find(config.id)) throw `a game with id ${config.id} already exists`;
      const g = game(config.id, config.name, config.location, { base_version: 0 });
      g.config = config;
      state.games.push(g);
      return g;
    },
    update_game: ({ config }) => { find(config.id).config = config; },
    remove_game: ({ id }) => { state.games = state.games.filter((g) => g.config.id !== id); },
    import_save: async ({ id }) => { await sleep(500); const g = find(id); g.base_version = g.staged.version; g.staged = null; },
    keep_local: async ({ id }) => { await sleep(500); const g = find(id); g.base_version = g.staged.version + 1; g.staged = null; },
    resolve_conflict: async ({ id }) => { await sleep(500); const g = find(id); g.base_version = g.conflict.remote.version + 1; g.conflict = null; },
    list_backups: ({ id }) => backups.filter((b) => b.game_id === id),
    restore_backup: async () => { await sleep(400); },
    sync_now: async () => { await sleep(900); },
    pick_folder: () => 'C:\\Users\\you\\Documents\\My Game\\saves',
    pick_app: () => 'game.exe',
    running_processes: () => ['emulator.exe', 'explorer.exe', 'game.exe', 'launcher.exe'],
    set_autostart: ({ enabled }) => { state.autostart = enabled; },
    check_for_updates: async () => { await sleep(700); state.update = { current: '1.0.0', state: 'ready', latest: '1.0.1', message: null }; return state.update; },
    install_update: () => {},
  };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        if (!commands[cmd]) throw `mock: unknown command ${cmd}`;
        return commands[cmd](args);
      },
    },
    event: { listen: async () => () => {} },
  };
})();
