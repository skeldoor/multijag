// Thin wrapper over Tauri's IPC so ui.js sees the same `api` as before.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const call = (cmd, args) => invoke(cmd, args).catch((e) => { throw new Error(typeof e === 'string' ? e : e.message || String(e)); });

window.api = {
  state: () => call('get_state'),
  addAccount: () => call('add_account'),
  removeAccount: (id) => call('remove_account', { id }),
  refreshCharacters: (id) => call('refresh_characters', { id }),
  launch: (id, characterId) => call('launch', { id, characterId }),
  reorderAccounts: (ids) => call('reorder_accounts', { ids }),
  reorderCharacters: (id, ids) => call('reorder_characters', { id, ids }),
  setHidden: (id, characterId, hidden) => call('set_hidden', { id, characterId, hidden }),
  checkUpdate: () => call('check_update'),
  openLink: (url) => call('open_link', { url }),
  setClient: (client) => call('set_client', { client }),
  browseClient: (client) => call('browse_client', { client }),
  onState: (fn) => listen('state', (e) => fn(e.payload)),
};
