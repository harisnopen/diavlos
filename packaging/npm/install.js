// Downloads the diavlos release binary for this platform into ./bin.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const https = require('https');
const zlib = require('zlib');
const { execFileSync } = require('child_process');

const REPO = 'harisnopen/diavlos';
const VERSION = process.env.DIAVLOS_VERSION || require('./package.json').version;

const os_ = { linux: 'unknown-linux-gnu', darwin: 'apple-darwin', win32: 'pc-windows-msvc' }[process.platform];
const arch = { x64: 'x86_64', arm64: 'aarch64' }[process.arch];
if (!os_ || !arch) { console.error(`diavlos: no binary for ${process.platform}/${process.arch}`); process.exit(1); }
const target = `${arch}-${os_}`;
const ext = process.platform === 'win32' ? 'zip' : 'tar.gz';
const url = `https://github.com/${REPO}/releases/download/v${VERSION}/diavlos-${target}.${ext}`;
const dest = path.join(__dirname, 'bin', process.platform === 'win32' ? 'diavlos.exe' : 'diavlos');

function get(u, cb) {
  https.get(u, { headers: { 'user-agent': 'diavlos-npm' } }, res => {
    if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) return get(res.headers.location, cb);
    if (res.statusCode !== 200) { console.error(`diavlos: download failed (${res.statusCode}) ${u}`); process.exit(1); }
    cb(res);
  }).on('error', e => { console.error(`diavlos: ${e.message}`); process.exit(1); });
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'diavlos-'));
const archive = path.join(tmp, `diavlos.${ext}`);
console.log(`diavlos: downloading ${url}`);
get(url, res => {
  const out = fs.createWriteStream(archive);
  res.pipe(out).on('finish', () => {
    if (ext === 'tar.gz') {
      execFileSync('tar', ['-xzf', archive, '-C', tmp]);
    } else {
      execFileSync('powershell', ['-NoProfile', '-Command', `Expand-Archive -Path '${archive}' -DestinationPath '${tmp}' -Force`]);
    }
    fs.copyFileSync(path.join(tmp, path.basename(dest)), dest);
    if (process.platform !== 'win32') fs.chmodSync(dest, 0o755);
    // The archive carries the licence notices next to the binary, and they
    // have to stay with it: this package is another copy. Releases before
    // 1.0.1 have no such files, so a missing one is not an error.
    for (const f of ['LICENSE', 'THIRD-PARTY-LICENSES.txt']) {
      const from = path.join(tmp, f);
      if (fs.existsSync(from)) fs.copyFileSync(from, path.join(__dirname, f));
    }
    fs.rmSync(tmp, { recursive: true, force: true });
    console.log(`diavlos: installed ${dest}`);
    void zlib;
  });
});
