import test from 'node:test';
import assert from 'node:assert/strict';
import { appSignals, filterApps, knownUsage } from './appFilters.ts';

const now = new Date(2026, 10, 17, 23, 30).getTime() / 1000;
const app = (extra = {}) => ({
  id: 'app', name: 'Example', kind: 'desktop', builtIn: false, removable: true,
  publisher: 'Example Vendor', version: '1', installDate: null, size: null,
  lastUsed: null, installedTogether: [], companions: [], installedAt: null, installLocation: null, needsAdmin: false, ...extra,
});
const dateAgo = (days) => {
  const date = new Date(now * 1000); date.setDate(date.getDate() - days);
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
};

test('unknown usage never means unused; Unix seconds and 90-day boundary', () => {
  for (const value of [null, undefined, NaN, Infinity, -1]) {
    assert.equal(knownUsage(value), false);
    assert.deepEqual(appSignals(app({ lastUsed: value }), now), ['unknown']);
  }
  assert.ok(appSignals(app({ lastUsed: now - 90 * 86400 }), now).includes('unused'));
  assert.ok(!appSignals(app({ lastUsed: now - 90 * 86400 + 1 }), now).includes('unused'));
  assert.ok(!appSignals(app({ lastUsed: now + 1 }), now).includes('unused'));
  assert.equal(knownUsage(0), true);
});

test('companion hints remain distinct from same-day peers and combine with other filters', () => {
  const bundled = app({ id: 'bundled', companions: ['Other app'], size: 1024 ** 3 });
  const peer = app({ id: 'peer', installedTogether: ['other'], size: 1024 ** 3 });
  assert.ok(appSignals(bundled, now).includes('companions'));
  assert.ok(!appSignals(bundled, now).includes('peers'));
  assert.ok(!appSignals(peer, now).includes('companions'));
  assert.deepEqual(filterApps([bundled, peer], '', 'desktop', { companions: true, large: true }, 'name', now).map(a => a.id), ['bundled']);
  assert.equal(Object.hasOwn(bundled, 'selected'), false);
});

test('large is inclusive at 500 MiB, not 500 MB', () => {
  assert.ok(appSignals(app({ size: 500 * 1024 * 1024 }), now).includes('large'));
  assert.ok(!appSignals(app({ size: 500 * 1024 * 1024 - 1 }), now).includes('large'));
  assert.ok(!appSignals(app({ size: 500e6 }), now).includes('large'));
});

test('recent uses calendar dates across month/DST boundaries, excludes invalid/future dates', () => {
  for (const days of [0, 1, 30]) assert.ok(appSignals(app({ installDate: dateAgo(days) }), now).includes('recent'));
  for (const date of [dateAgo(31), dateAgo(-1), '2026-02-30', 'bad', null]) {
    assert.ok(!appSignals(app({ installDate: date }), now).includes('recent'));
  }
});

test('peer signals exclude self and do not imply a selection or deletion', () => {
  assert.ok(!appSignals(app({ installedTogether: ['app'] }), now).includes('peers'));
  const target = Object.freeze(app({ installedTogether: ['peer'] }));
  assert.ok(appSignals(target, now).includes('peers'));
  assert.equal(Object.hasOwn(target, 'selected'), false);
});

test('advanced signals intersect and retain query/type/built-in filters', () => {
  const targets = [
    app({ id: 'large', size: 600e6, installDate: dateAgo(2), installedTogether: ['peer'] }),
    app({ id: 'small', size: 1, installDate: dateAgo(2), installedTogether: ['peer'] }),
    app({ id: 'store', kind: 'store', builtIn: true, size: 600e6, installDate: dateAgo(2), installedTogether: ['peer'] }),
  ];
  const signals = { large: true, recent: true, unknown: true, peers: true };
  assert.deepEqual(filterApps(targets, ' vendor ', 'desktop', signals, 'name', now).map((a) => a.id), ['large']);
  assert.deepEqual(filterApps(targets, '', 'builtIn', signals, 'name', now).map((a) => a.id), ['store']);
  assert.equal(filterApps(targets, '', 'all', { unknown: true, unused: true }, 'name', now).length, 0);
  assert.equal(filterApps(targets, 'missing', 'all', {}, 'name', now).length, 0);
});

test('table sort directions keep unknown metadata last', () => {
  const targets = [
    app({ id: 'unknown', name: 'A' }),
    app({ id: 'new', name: 'C', lastUsed: now, size: 20, installDate: '2026-11-17' }),
    app({ id: 'old', name: 'B', lastUsed: 0, size: 0, installDate: '2026-01-01' }),
  ];
  for (const sort of ['lastUsed', 'size', 'date']) {
    assert.deepEqual(filterApps(targets, '', 'all', {}, sort, now, 'asc').map((a) => a.id), ['old', 'new', 'unknown']);
    assert.deepEqual(filterApps(targets, '', 'all', {}, sort, now, 'desc').map((a) => a.id), ['new', 'old', 'unknown']);
  }
  assert.deepEqual(filterApps(targets, '', 'all', {}, 'name', now, 'desc').map((a) => a.id), ['new', 'old', 'unknown']);
});

test('last observed sort puts oldest first and unknown last without mutating input', () => {
  const targets = [app({ id: 'unknown', name: 'A' }), app({ id: 'new', lastUsed: now }), app({ id: 'old', lastUsed: 0 })];
  const snapshot = structuredClone(targets);
  assert.deepEqual(filterApps(targets, '', 'all', {}, 'lastUsed', now).map((a) => a.id), ['old', 'new', 'unknown']);
  assert.deepEqual(targets, snapshot);
});
