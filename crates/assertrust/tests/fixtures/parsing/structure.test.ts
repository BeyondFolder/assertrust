// Test structure: describe blocks, test variants, hooks, tables.
import { afterAll, beforeEach, describe, expect, it, test } from 'vitest';

beforeEach(() => {
  expect(globalThis.db).toBeDefined();
});

describe('billing', () => {
  beforeEach(() => {
    resetPlans();
  });

  it('charges the card', () => {
    expect(charge(10)).toBe(true);
  });

  it.skip('refunds the card', () => {
    expect(refund(10)).toBe(true);
  });

  it.todo('sends a receipt');

  it.each(['pro', 'agency'])('upgrades to %s', (plan) => {
    expect(upgrade(plan)).toEqual({ plan, ok: true });
  });

  test.skipIf(process.env.CI)('talks to the real API', async () => {
    expect(await ping()).toBe('pong');
  });

  describe.skip('invoices', () => {
    test('lists invoices', () => {
      expect(invoices()).toHaveLength(2);
    });

    test.only(`totals ${'EUR'} invoices`, () => {
      expect(total('EUR')).toBe(30);
    });
  });
});

describe.each([
  { currency: 'EUR', symbol: '€' },
  { currency: 'USD', symbol: '$' },
])('formats $currency', ({ currency, symbol }) => {
  it('prefixes the symbol', () => {
    expect(format(1, currency)).toMatch(symbol);
  });
});

xit('is disabled the old way', () => {
  expect(1).toBe(1);
});

afterAll(() => {
  cleanup();
});
