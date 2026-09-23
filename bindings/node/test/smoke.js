// Smoke test: a home-made Node bot joins a room made by the CLI and
// finishes a task. Needs the diavlos binary (DIAVLOS_BIN or on PATH).
'use strict';
const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const { Room, DiavlosError } = require('..');

const BIN = process.env.DIAVLOS_BIN || 'diavlos';

function cli(home, args, user = 'haris') {
  const r = spawnSync(BIN, args, { env: Object.assign({}, process.env, { DIAVLOS_HOME: home, USER: user, USERNAME: user }), encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`cli ${args.join(' ')} failed: ${r.stderr}`);
  return r.stdout;
}

async function main() {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'diavlos-node-'));
  const a = path.join(base, 'a'), b = path.join(base, 'b');
  for (const h of [a, b]) { fs.mkdirSync(h); fs.writeFileSync(path.join(h, 'config.toml'), '[helper]\npublic_relays = false\nretry_secs = 1\n'); }
  try {
    cli(a, ['new', 'ops']);
    const note = cli(a, ['invite', 'ops', 'nodebot']);
    const invite = note.split(/\s+/).find(w => w.startsWith('dv1.'));

    const room = await Room.join(invite, { name: 'nodebot', home: b });
    assert.strictEqual(room.me, 'nodebot');
    const m = await room.send('hello from node');
    assert.strictEqual(m.from, 'nodebot');

    const got = cli(a, ['next', 'ops', '--timeout', '20']);
    assert.ok(got.includes('nodebot (chat): hello from node'), got);

    cli(a, ['send', 'ops', 'please do y', '--type', 'task']);
    for await (const msg of room.messages(20)) {
      if (msg.type === 'task') {
        const done = await room.send('did y', { type: 'done', reply_to: msg.id });
        assert.strictEqual(done.reply_to, msg.id);
        await room.ack(msg); // breaking out: settle this one ourselves
        break;
      }
    }

    // A message held and never acked comes round again.
    cli(a, ['send', 'ops', 'again?', '--type', 'task']);
    const held = await room.next(20, { ack: false, lease: 1 });
    assert.strictEqual(held.delivery.attempt, 1);
    await new Promise(r => setTimeout(r, 2000));
    const back = await room.next(20, { ack: false });
    assert.strictEqual(back.id, held.id);
    assert.strictEqual(back.delivery.attempt, 2);
    await assert.rejects(room.ack(held), e => e.code === 6);
    await room.ack(back);
    const who = await room.who();
    assert.deepStrictEqual(who.map(w => w.name).sort(), ['haris', 'nodebot']);

    await assert.rejects(room.next(1), e => e instanceof DiavlosError && e.code === 4);
    await assert.rejects(Room.open('nope', { name: 'nodebot', home: b }).send('x'), e => e.code === 2);

    const log = await room.read(1, 100);
    assert.ok(log.some(x => x.text === 'did y'));
    console.log('node smoke: ok');
  } finally {
    for (const h of [a, b]) spawnSync(BIN, ['--home', h, 'stop']);
  }
}

main().catch(e => { console.error(e); process.exit(1); });
