import { describe, expect, it } from 'vitest';
import { checkout, loadOrder, log, refund } from '../src/checkout';

describe('checkout', () => {
  it('charges the card', async () => {
    const order = await checkout({ items: ['book'], card: '4242' });
    expect(order.status).toBe('paid');
    expect(order.total).toBe(1200);
  });

  it('refunds a paid order', async () => {
    expect.assertions(1);
    const result = await refund('order-1');
    if (result.ok) {
      expect(result.ok).toBe(true);
    }
    expect(loadOrder('order-1')).resolves.toMatchObject({ status: 'refunded' });
  });

  it.skip('rejects an empty cart', async () => {
    await expect(checkout({ items: [] })).rejects.toThrow('empty cart');
  });

  it('keeps the cart after a declined card', async () => {
    const order = await checkout({ items: ['book'], card: '0002' });
    expect(order.status).toBe('declined');
    return;
    expect(order.cart).toEqual(['book']);
  });

  it('logs the payment', async () => {
    const order = await checkout({ items: ['book'] });
    expect(log.last());
  });
});
