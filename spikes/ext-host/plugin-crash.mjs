// plugin-crash.mjs — dies on initialize: process.exit(1)
import readline from 'node:readline';
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    console.error('plugin-crash: boom on initialize');
    process.exit(1);
  }
});
