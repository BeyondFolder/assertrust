// Before: matched against billing.after.test.ts by `assertrust pairs`.
import { describe, expect, it } from 'vitest';

describe('invoices', () => {
  it('totals the lines', () => {
    expect(total([1, 2])).toBe(3);
  });

  it('stores the pending invoice', async () => {
    const res = await createInvoice({ amount: 10 });
    expect(res.status).toBe(201);
    expect(res.body).toMatchObject({ amount: 10, currency: 'EUR', status: 'pending' });
  });

  it('marks an invoice as paid', async () => {
    const invoice = await pay(7);
    expect(invoice.status).toBe('paid');
    expect(invoice.paidAt).toBeDefined();
  });

  it('sends the receipt', async () => {
    const mail = await receiptFor(7);
    expect(mail.to).toBe('ada@example.test');
    expect(mail.subject).toBe('Your receipt');
  });

  it('formats the amount for the customer', () => {
    expect(formatAmount({ cents: 1050, currency: 'EUR' }).text).toBe('€10.50');
    expect(formatAmount({ cents: 99, currency: 'EUR' }).text).toBe('€0.99');
  });

  it('refunds a paid invoice', async () => {
    expect(await refund(7)).toEqual({ ok: true });
    expect(await balance(7)).toBe(0);
  });

  it('rejects a negative amount', async () => {
    await expect(createInvoice({ amount: -1 })).rejects.toThrow('amount');
  });
});

describe('reminders', () => {
  it('waits three days before the first reminder', () => {
    expect(nextReminder(day(0))).toEqual(day(3));
  });
});
