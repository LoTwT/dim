import assert from 'node:assert/strict';
import {test} from 'node:test';
import {plan} from './ci-test-concurrency.mjs';

test('the plan scales by physical cores', () => {
  assert.deepEqual(plan('linux', 32, 16, 32), {jobs: 5, threads: 4}, 'sixteen physical cores');
  assert.deepEqual(plan('linux', 8, 8, 8), {jobs: 3, threads: 2}, 'no SMT siblings');
});

test('the plan falls back to the CPU count without a topology', () => {
  assert.deepEqual(plan('linux', 16, 0, 0), {jobs: 5, threads: 4});
  assert.deepEqual(plan('linux', 4, 0, 0), {jobs: 3, threads: 2});
});

test('the plan holds the macOS settings on other platforms', () => {
  assert.deepEqual(plan('darwin', 32, 16, 32), {jobs: 3, threads: 3});
});
