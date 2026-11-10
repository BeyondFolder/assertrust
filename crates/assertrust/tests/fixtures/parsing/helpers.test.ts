import { describe, expect, it } from 'vitest';
import { seed, type Row } from './support';
import expectSorted from './sorted';
export * from './shared';

function expectOk(res: Response, status = 200) {
  expect(res.status).toBe(status);
  expect(res.ok).toBe(true);
}

const expectRow = (row: Row, { strict }: { strict: boolean }) => {
  if (strict) expect(row).toEqual(seed());
};

async function expectSaved(id: string) {
  expect(await db.get(id)).toBeDefined();
}

describe('rows', () => {
  function expectEmpty(rows: Row[]) {
    expect(rows).toHaveLength(0);
  }

  it('lists', async () => {
    expectOk(await list());
    expectEmpty(await rows());
    expectSorted(await rows());
    await expectSaved('a');
  });

  it('stores', () => {
    const row = seed();
    expectRow(row, { strict: true });
    console.log(row);
  });
});
