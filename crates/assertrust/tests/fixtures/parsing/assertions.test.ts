// The shapes of a check: matchers, modifiers and arguments.
import { expect, it } from 'vitest';

const PLAN = 'pro';
const LIMITS = { seats: 3, projects: 10 };

expect.extend({
  toBeWithinRange(received: number, floor: number, ceiling: number) {
    return { pass: received >= floor && received <= ceiling, message: () => '' };
  },
});

it('compares values', () => {
  expect(add(1, 2)).toBe(3);
  expect(user).toEqual({ name: "Ada", roles: ['admin', 'dev',], });
  expect(user).toStrictEqual({ roles: ['admin', 'dev'], name: 'Ada' });
  expect(res.body).toMatchObject({ email: 'me@example.test', status: 'pending', domainIds: null });
  expect(plan).toBe(PLAN);
  expect(limits).toEqual(LIMITS);
  expect(count).toBe(0x10);
  expect(delta).toBeCloseTo(-0.5, 2);
});

it('checks text and lists', () => {
  expect(html).not.toMatch(/support@|resend|inboxes\.net/i);
  expect(tags).toContain('urgent');
  expect(list).toHaveLength(2);
  expect(order).toHaveProperty('total', 30);
  expect(value).toBeTruthy();
  expect(error).toBeInstanceOf(TypeError);
});

it('uses nested matchers', () => {
  expect(event).toEqual({
    id: expect.any(String),
    payload: expect.objectContaining({ kind: 'email' }),
    tags: expect.arrayContaining(['a']),
    note: expect.not.stringContaining('secret'),
  });
});

it('waits for promises', async () => {
  await expect(load()).resolves.toEqual({ ok: true });
  await expect(save()).rejects.toThrow('disk full');
  expect(load()).resolves.toBeDefined();
  expect.soft(header).toBe('x');
  await expect.poll(() => status()).toBe('done');
});

it('counts and spies', () => {
  expect.assertions(2);
  expect.hasAssertions();
  expect(send).toHaveBeenCalledWith('me@example.test', expect.anything());
  expect(send).toHaveBeenCalledTimes(1);
  expect(render()).toMatchInlineSnapshot(`"<p>hi</p>"`);
  expect(score).toBeWithinRange(1, 10);
});

it('checks nothing', () => {
  expect(result);
  expect(other).not;
});
