// Where a check sits: places where it may not run, or may not fail the test.
import { expect, it } from 'vitest';

it('checks conditionally', () => {
  const res = send();
  if (res.ok) {
    expect(res.status).toBe(200);
  } else {
    expect(res.error).toBeDefined();
  }
  res.retried && expect(res.attempts).toBeGreaterThan(1);
  res.cached ? expect(res.age).toBeLessThan(60) : null;
});

it('checks in loops and callbacks', () => {
  for (const item of items) {
    expect(item.valid).toBe(true);
  }
  items.forEach((item) => {
    expect(item.id).toBeDefined();
  });
  setTimeout(() => expect(done).toBe(true), 10);
});

it('checks around errors', async () => {
  try {
    await risky();
    expect(state).toBe('saved');
  } catch (error) {
    expect(error.message).toMatch(/timeout/);
  } finally {
    expect(lock.released).toBe(true);
  }
});

it('returns early', () => {
  if (!featureEnabled) return;
  expect(feature()).toBe('on');
  return expect(flush()).resolves.toBe(true);
});

it('skips itself', (ctx) => {
  if (process.env.CI) ctx.skip();
  expect(local()).toBe(true);
});

it('skips with a destructured skip', ({ skip }) => {
  skip(!hasNetwork, 'needs network');
  expect(fetchIt()).toBeTruthy();
});
