import { describe, expect, it } from 'vitest';
import { checkout, loadOrder, log, refund } from '../src/checkout';

describe('checkout', () => {
  it('charges the card and emails a receipt', async () => {
    const order = await checkout({ items: ['book'], card: '4242' });
    expect(order.status).toBe('paid');
    expect(order.total).toBe(1200);
    expect(order.receipt).toMatchObject({ sentTo: 'ada@example.com' });
  });

  it('refunds a paid order', async () => {
    expect.assertions(2);
    const result = await refund('order-1');
    expect(result.ok).toBe(true);
    await expect(loadOrder('order-1')).resolves.toMatchObject({ status: 'refunded' });
  });

  it('rejects an empty cart', async () => {
    await expect(checkout({ items: [] })).rejects.toThrow('empty cart');
  });

  it('applies a coupon', async () => {
    const order = await checkout({ items: ['book'], coupon: 'HALF' });
    expect(order.total).toBe(600);
  });

  it('keeps the cart after a declined card', async () => {
    const order = await checkout({ items: ['book'], card: '0002' });
    expect(order.status).toBe('declined');
    expect(order.cart).toEqual(['book']);
  });

  it('logs the payment', async () => {
    const order = await checkout({ items: ['book'] });
    expect(log.last()).toContain(order.id);
  });

  it('saves the order', async () => {
    const order = await checkout({ items: ['book'] });
    expect(order).toMatchObject({ status: 'paid', currency: 'EUR', items: ['book'] });
    expect(order.total).toBe(1200);
    expect(order.paidAt).toEqual(new Date('2026-01-01'));
  });
});
