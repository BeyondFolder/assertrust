// After: matched against billing.before.test.ts by `assertrust pairs`.
import { describe, expect, it } from 'vitest';

describe('invoices', () => {
  it('totals the lines', () => {
    expect(total([1, 2])).toBe(3);
  });

  it('stores the pending invoice', async () => {
    const res = await createInvoice({ amount: 10 });
    expect(res.status).toBe(201);
    expect(res.body).toMatchObject({ amount: 10 });
  });

  it('marks a paid invoice', async () => {
    const invoice = await pay(7);
    expect(invoice).toMatchObject({ status: 'paid', method: 'card' });
    expect(invoice.paidAt).toBeDefined();
  });

  it('sends the receipt', async () => {
    const mail = await receiptFor(7);
    expect(mail.to).toBe('ada@example.test');
  });

  it('formats the amount for the customer', () => {
    expect(money(1050, 'EUR').text).toBe('€10.50');
    expect(money(99, 'EUR').text).toBe('€0.99');
  });

  it('refunds a paid invoice', async () => {
    expect(await refund(7)).toEqual({ ok: true });
  });

  it('keeps the balance at zero after a refund', async () => {
    await refund(7);
    expect(await balance(7)).toBe(0);
  });
});

describe('reminders', () => {
  describe('schedule', () => {
    it('waits three days before the first reminder', () => {
      expect(nextReminder(day(0))).toEqual(day(3));
    });
  });
});
