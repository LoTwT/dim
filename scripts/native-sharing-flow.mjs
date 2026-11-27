import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { connect } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';

const cli = path.resolve(process.env.DIM_SHARING_TEST_CLI || 'target/debug/dimcode');

async function rpc(socket) {
  const stream = connect(socket);
  await once(stream, 'connect');
  const lines = createInterface({ input: stream, crlfDelay: Infinity })[Symbol.asyncIterator]();
  let id = 0;
  async function receive(matches) {
    for (;;) {
      const { value, done } = await lines.next();
      if (done) throw Error('App Server stream ended');
      const reply = JSON.parse(value);
      if (matches(reply)) return reply;
    }
  }
  function call(method, params) {
    const request = ++id;
    stream.write(JSON.stringify({ jsonrpc: '2.0', id: request, method, params }) + '\n');
    return receive(reply => reply.id === request);
  }
  const { result: hello } = await call('hello', { protocolVersion: 4, clientKind: 'cli', clientName: 'sharing-test' });
  return { stream, call, hello, receive };
}

test('a remembered shared link carries independent native streams, large frames and revocation', { skip: process.platform === 'win32', timeout: 50_000 }, async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'dim-net-'));
  const a = path.join(temp, 'a'), b = path.join(temp, 'b'), tmp = path.join(temp, 'tmp');
  const target = path.join(b, 'rust', 'daemon.sock');
  const env = { ...process.env, HOME: temp, TMPDIR: tmp, DIMCODE_HOME: b, DIMCODE_UPDATE_CHECK: '0', DIMCODE_SHARING_LISTEN: '127.0.0.1:0' };
  const children = new Set();
  function start(args) {
    const child = spawn(cli, args, { env, cwd: temp, stdio: 'pipe' });
    children.add(child);
    child.on('close', () => children.delete(child));
    return child;
  }
  async function run(args, input = '') {
    const child = start(args), out = [], err = [];
    child.stdout.on('data', chunk => out.push(chunk));
    child.stderr.on('data', chunk => err.push(chunk));
    child.stdin.end(input);
    const [code] = await once(child, 'close');
    return { code, out: Buffer.concat(out).toString(), err: Buffer.concat(err).toString() };
  }
  async function share(...args) {
    const result = await run(['--home', b, '--model-config', path.resolve(import.meta.dirname, '../crates/dim-agent-aimux/examples/offline.json'), 'share', ...args]);
    assert.equal(result.code, 0, result.err);
    return JSON.parse(result.out);
  }
  t.after(async () => {
    for (const child of children) child.kill();
    await Promise.allSettled([...children].map(child => once(child, 'close')));
    await rm(temp, { recursive: true, force: true });
  });
  await mkdir(path.dirname(target), { recursive: true, mode: 0o700 });
  await chmod(path.dirname(target), 0o700);
  await mkdir(tmp, { mode: 0o700 });
  const model = path.resolve(import.meta.dirname, '../crates/dim-agent-aimux/examples/offline.json');
  const disabled = await run(['--home', a, 'share', 'disable']);
  assert.equal(disabled.code, 0, disabled.err);
  assert.equal(JSON.parse(disabled.out).enabled, false);
  await assert.rejects(readFile(path.join(a, 'rust', 'daemon.sock')), { code: 'ENOENT' });
  const helper = path.join(temp, 'sharing-helper');
  await writeFile(helper, '', { mode: 0o600 });
  env.DIM_TEST_SHARING_HELPER = path.join(path.dirname(cli), 'dim-sharing');
  const server = start(['app-server', '--sharing-helper', helper, '--home', path.dirname(target), '--model-config', model, '--no-ws', '--unix', target]);
  const diagnostics = createInterface({ input: server.stderr })[Symbol.asyncIterator]();
  async function waitForDiagnostic(message) {
    for (;;) {
      const { value, done } = await diagnostics.next();
      assert.ok(!done, 'the sharing supervisor stopped before ' + message);
      if (value.includes(message)) return;
    }
  }
  await Promise.all([once(server.stdout, 'data'), waitForDiagnostic('Remote sharing could not start')]);
  const direct = await rpc(target);
  t.after(() => direct.stream.destroy());
  await writeFile(helper, [
    '#!/bin/sh', 'if [ ! -e "$2/restart-check" ]; then',
    '  touch "$2/restart-check"', '  exit 1', 'fi',
    'echo "starting repaired helper" >&2', 'exec "$DIM_TEST_SHARING_HELPER" "$@"', '',
  ].join('\n'));
  await chmod(helper, 0o700);
  await waitForDiagnostic('starting repaired helper');
  assert.equal((await share('status')).enabled, false);
  const { token, addresses: [address] } = await share('enable');
  const clientArgs = ['--home', a, 'share', 'connect', address];
  assert.notEqual((await run([...clientArgs, '--', 'mcp', 'list'], 'incorrect\n')).code, 0);

  const hold = start([...clientArgs, '--', '--no-tui']);
  hold.stdin.write(token + '\n');
  let proxy;
  for (let i = 0; i < 200 && !proxy; i++) {
    for (const entry of await readdir(tmp, { withFileTypes: true })) {
      if (entry.isDirectory() && entry.name.startsWith('dim-link-')) {
        const dir = path.join(tmp, entry.name);
        if ((await readdir(dir)).includes('ready')) proxy = path.join(dir, 'rpc.sock');
      }
    }
    if (!proxy) await delay(50);
  }
  assert.ok(proxy, 'the authenticated client must publish its private socket');
  const first = await rpc(proxy), second = await rpc(proxy);
  t.after(() => { first.stream.destroy(); second.stream.destroy(); });
  const database = path.join(temp, 'keyboard.sqlite');
  const localOutput = path.join(temp, 'timeline.json');
  await writeFile(database, 'local fixture');
  for (const args of [
    ['daemon', 'vacuum', '--database', database], ['export', 'fixture-session', '--database', database, '--output', localOutput],
    ['migration', 'list'], ['update'], ['web', 'install'], ['share', 'enable'], ['--fullscreen', 'default'],
  ]) {
    const refused = await run([...clientArgs, '--', ...args]);
    assert.notEqual(refused.code, 0);
    assert.match(refused.err, /unavailable through a shared connection/, args.join(' '));
  }
  assert.equal(await readFile(database, 'utf8'), 'local fixture');
  for (const file of [localOutput, path.join(a, 'config.json'), path.join(a, 'rust', 'daemon.sock')]) {
    await assert.rejects(readFile(file), { code: 'ENOENT' });
  }
  assert.equal(first.hello.instanceId, direct.hello.instanceId);
  assert.deepEqual(first.hello.features, direct.hello.features);
  for (let attempt = 0; attempt < 50; attempt++) {
    const { result } = await direct.call('mcp.catalog', {});
    if (result.userApplication.appliedRevision === result.userRevision && result.pluginApplication.appliedRevision) break;
    await delay(50);
  }
  for (const [method, params] of [['mcp.catalog', {}], ['plugin.list', {}], ['sharing.absent', {}]]) {
    const { id: _local, ...local } = await direct.call(method, params);
    const { id: _remote, ...remote } = await first.call(method, params);
    assert.deepEqual(remote, local);
  }
  const { result: session } = await direct.call('session.create', { cwd: temp });
  assert.equal((await first.call('session.attach', { sessionId: session.sessionId })).result.sessionId, session.sessionId);
  const notice = first.receive(message => message.method === 'session.event' && message.params.sessionId === session.sessionId);
  assert.ok((await direct.call('session.submit', { sessionId: session.sessionId, text: 'Hello' })).result);
  await notice;
  first.stream.destroy();
  const large = { key: 'x'.repeat(1024 * 1024) };
  const expected = await direct.call('sharing.absent', large);
  const actual = await second.call('sharing.absent', large);
  assert.deepEqual(actual.error, expected.error);
  const pendingClose = once(second.stream, 'close');
  await share('reset');
  await pendingClose;
  hold.stdin.end();
  const stale = await run([...clientArgs, '--', 'mcp', 'list']);
  assert.notEqual(stale.code, 0);
  assert.doesNotMatch(stale.err, /Sharing token: /, 'reconnect must try its saved credential');
  const current = await share('status');
  assert.notEqual(current.token, token);
  const repaired = await run([...clientArgs, '--pair', '--', 'mcp', 'list'], current.token + '\n');
  assert.equal(repaired.code, 0, repaired.err);
  const reconnected = await run([...clientArgs, '--', 'mcp', 'list']);
  assert.equal(reconnected.code, 0, reconnected.err);
  assert.doesNotMatch(reconnected.err, /Sharing token: /);
  const alternateAddress = address.replace('127.0.0.1', 'localhost');
  const alternate = await run(['--home', a, 'share', 'connect', alternateAddress, '--', 'mcp', 'list']);
  assert.equal(alternate.code, 0, alternate.err);
  assert.doesNotMatch(alternate.err, /Sharing token: /, 'a different route to the same authenticated instance can reconnect');
  assert.deepEqual((await share('disable')).addresses, []);
});
