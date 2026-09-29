// plugin.mjs — reference plugin: JSON-RPC 2.0 over stdio (NDJSON, one message per line)
import readline from 'node:readline';

const METHODS = {
  initialize: async () => ({ ok: true, commands: ['demo'] }),
  onQuery: async (params) => ({
    items: [
      { id: 1, title: 'Demo item one', subtitle: `q="${params?.q ?? ''}"`, action: 'copy' },
      { id: 2, title: 'Demo item two', subtitle: 'fake result #2', action: 'open' },
      { id: 3, title: 'Demo item three', subtitle: 'fake result #3', action: 'none' },
    ],
  }),
};

function send(id, result, error) {
  const msg = { jsonrpc: '2.0', id };
  if (error) msg.error = error; else msg.result = result;
  process.stdout.write(JSON.stringify(msg) + '\n');
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); } catch { return send(null, undefined, { code: -32700, message: 'Parse error' }); }
  const { id, method, params } = msg;
  const handler = METHODS[method];
  if (!handler) return send(id, undefined, { code: -32601, message: `Method not found: ${method}` });
  Promise.resolve(handler(params)).then(
    (result) => send(id, result),
    (err) => send(id, undefined, { code: -32000, message: String(err?.message ?? err) }),
  );
});
