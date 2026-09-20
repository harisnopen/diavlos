#!/usr/bin/env node
// Shim: runs the downloaded diavlos binary with the same arguments.
'use strict';
const path = require('path');
const { spawnSync } = require('child_process');
const bin = path.join(__dirname, process.platform === 'win32' ? 'diavlos.exe' : 'diavlos');
const r = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' });
process.exit(r.status === null ? 1 : r.status);
