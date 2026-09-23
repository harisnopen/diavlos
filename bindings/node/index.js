// Diavlos: the channel between AI agents. Node binding.
// The same calls as the MCP tools: send, ask, next, read, claim, release,
// who, rooms, and ack, renew and nack for messages you hold. Talks to the
// local helper; starts it if needed.
//
// Messages from messages() and watch() are held for you: each stays yours
// until acknowledged, and comes round again if it never is. Both loops
// acknowledge a message when you ask for the next one. If you break out of
// a loop after finishing a message, call room.ack(msg) first.
'use strict';
const net = require('net');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');

const CODES = { 2: 'not in room', 3: 'reached nobody', 4: 'timed out', 5: 'name already taken', 6: 'denied', 7: 'room paused' };

class DiavlosError extends Error {
  constructor(code, message) { super(message); this.code = code; this.name = 'DiavlosError'; }
  toString() { return `${CODES[this.code] || 'error'} (${this.code}): ${this.message}`; }
}

function defaultHome() {
  return process.env.DIAVLOS_HOME || path.join(os.homedir(), '.diavlos');
}

function socketPath(home) {
  if (process.platform === 'win32') {
    const tag = crypto.createHash('sha256').update(home).digest('hex').slice(0, 12);
    return `\\\\.\\pipe\\diavlos-${tag}`;
  }
  return path.join(home, 'helper.sock');
}

function connectOnce(home) {
  return new Promise((resolve, reject) => {
    const s = net.connect(socketPath(home));
    s.once('connect', () => resolve(s));
    s.once('error', reject);
  });
}

function binary() {
  return process.env.DIAVLOS_BIN || 'diavlos';
}

function spawnHelper(home) {
  const child = spawn(binary(), ['--home', home, 'helper'], { detached: true, stdio: 'ignore', windowsHide: true });
  child.unref();
}

async function connect(home) {
  try { return await connectOnce(home); } catch (_) { /* start it */ }
  spawnHelper(home);
  const deadline = Date.now() + 10000;
  for (;;) {
    await new Promise(r => setTimeout(r, 100));
    try { return await connectOnce(home); } catch (e) {
      if (Date.now() > deadline) throw new DiavlosError(1, `helper did not start: ${e.message}`);
    }
  }
}

function unwrap(line) {
  const resp = JSON.parse(line);
  if (resp.ok) return resp.result === undefined ? null : resp.result;
  throw new DiavlosError(resp.code || 1, resp.error || 'unknown error');
}

async function call(home, op, fields) {
  const s = await connect(home);
  return new Promise((resolve, reject) => {
    let buf = '';
    s.on('data', d => {
      buf += d.toString('utf8');
      const i = buf.indexOf('\n');
      if (i >= 0) {
        const line = buf.slice(0, i);
        s.destroy();
        try { resolve(unwrap(line)); } catch (e) { reject(e); }
      }
    });
    s.on('error', reject);
    s.on('close', () => reject(new DiavlosError(1, 'helper closed the connection')));
    s.write(JSON.stringify(Object.assign({ op }, fields)) + '\n');
  });
}

async function* stream(home, op, fields) {
  const s = await connect(home);
  s.write(JSON.stringify(Object.assign({ op }, fields)) + '\n');
  let buf = '';
  const queue = [];
  let wake = null, done = false, err = null;
  s.on('data', d => {
    buf += d.toString('utf8');
    let i;
    while ((i = buf.indexOf('\n')) >= 0) { queue.push(buf.slice(0, i)); buf = buf.slice(i + 1); }
    if (wake) { wake(); wake = null; }
  });
  s.on('error', e => { err = e; done = true; if (wake) { wake(); wake = null; } });
  s.on('close', () => { done = true; if (wake) { wake(); wake = null; } });
  try {
    for (;;) {
      if (queue.length) { yield unwrap(queue.shift()); continue; }
      if (done) { if (err) throw err; return; }
      await new Promise(r => { wake = r; });
    }
  } finally { s.destroy(); }
}

class Room {
  constructor(room, opts) {
    opts = opts || {};
    this.room = room;
    this.identity = opts.name || 'default';
    this.home = opts.home || defaultHome();
    this.me = null;
  }
  static async join(invite, opts) {
    opts = opts || {};
    const home = opts.home || defaultHome();
    const r = await call(home, 'join', { invite, identity: opts.name || 'default' });
    const room = new Room(r.room.name, { name: opts.name, home });
    room.me = r.name;
    return room;
  }
  static open(room, opts) { return new Room(room, opts); }
  async send(text, opts) {
    opts = opts || {};
    let r;
    if (opts.type === 'approve' && opts.reply_to) {
      r = await call(this.home, 'approve', { room: this.room, identity: this.identity, msg_id: opts.reply_to });
    } else if (opts.type === 'deny' && opts.reply_to) {
      r = await call(this.home, 'deny', { room: this.room, identity: this.identity, msg_id: opts.reply_to, reason: text });
    } else {
      const draft = { text, type: opts.type || 'chat', to: opts.to || null, reply_to: opts.reply_to || null, trace: opts.trace || null, data: opts.data === undefined ? null : opts.data };
      r = await call(this.home, 'send', { room: this.room, identity: this.identity, draft });
    }
    return r.message;
  }
  async ask(text, opts) {
    opts = opts || {};
    const draft = { text, type: 'question', action: opts.action || null, trace: opts.trace || null };
    const r = await call(this.home, 'ask', { room: this.room, identity: this.identity, draft, timeout_secs: opts.timeout === undefined ? 120 : opts.timeout });
    return r.reply;
  }
  // The next message from someone else. With { ack: false } it stays yours
  // until ack/nack or the lease ({ lease } seconds, default 600) runs out;
  // msg.delivery holds the token.
  async next(timeout, opts) {
    opts = opts || {};
    const manual = opts.ack === false;
    const r = await call(this.home, 'next', { room: this.room, identity: this.identity, timeout_secs: timeout || 0, manual_ack: manual, lease_secs: opts.lease === undefined ? null : opts.lease });
    if (!manual) return r;
    return Object.assign(r.message, { delivery: r.delivery });
  }
  async *messages(timeout) {
    for (;;) {
      const msg = await this.next(timeout, { ack: false });
      yield msg;
      await this.ack(msg);
    }
  }
  // Taken on. Not "finished": say done in the room for that.
  ack(msg) { return call(this.home, 'ack', { identity: this.identity, token: token(msg) }); }
  renew(msg, lease) { return call(this.home, 'renew', { identity: this.identity, token: token(msg), lease_secs: lease === undefined ? null : lease }); }
  nack(msg, retryIn) { return call(this.home, 'nack', { identity: this.identity, token: token(msg), retry_in_secs: retryIn === undefined ? 60 : retryIn }); }
  // Look only: moves nothing unless ack is true.
  async read(since, limit, ack) {
    const r = await call(this.home, 'read', { room: this.room, identity: this.identity, since: since === undefined ? null : since, limit: limit || 50, ack: !!ack });
    return r.messages;
  }
  async claim(taskId) { return (await call(this.home, 'claim', { room: this.room, identity: this.identity, task_id: taskId })).message; }
  async release(taskId) { return (await call(this.home, 'release', { room: this.room, identity: this.identity, task_id: taskId })).message; }
  who() { return call(this.home, 'who', { room: this.room }); }
  async *watch() {
    for await (const item of stream(this.home, 'watch', { room: this.room, identity: this.identity, manual_ack: true })) {
      const msg = Object.assign(item.message, { delivery: item.delivery });
      yield msg;
      await this.ack(msg);
    }
  }
}

function token(msg) {
  if (typeof msg === 'string') return msg;
  if (msg && msg.delivery && msg.delivery.token) return msg.delivery.token;
  throw new DiavlosError(1, 'that message was not held for you: take it with next(t, { ack: false }), messages() or watch()');
}

function rooms(home) { return call(home || defaultHome(), 'rooms', {}); }

module.exports = { Room, rooms, DiavlosError };
