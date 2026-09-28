/**
 * `items` in order, each handed out with what `start(item)` returned, started ahead of
 * the consumer: before an item is handed out, `start` has been called for up to `size`
 * items after it, so a consumer that works through them one at a time finds the next
 * `size` already started. Each item is started once, in order, and never more than
 * `size` ahead of the one handed out: nothing starts before the first item is asked
 * for, and nothing more once the consumer stops asking.
 *
 * `size` must be a positive integer (a `RangeError` otherwise): a window of none would
 * hand out nothing.
 */
export function* slidingWindow<T, R>(
  items: Iterable<T>,
  size: number,
  start: (item: T) => R,
): Generator<readonly [T, R], void, undefined> {
  if (!Number.isSafeInteger(size) || size < 1) {
    throw new RangeError(`sliding window size must be a positive integer, got ${size}`);
  }
  const upcoming = items[Symbol.iterator]();
  const started: Array<readonly [T, R]> = [];
  const refill = (): void => {
    while (started.length < size) {
      const step = upcoming.next();
      if (step.done === true) {
        return;
      }
      started.push([step.value, start(step.value)]);
    }
  };
  refill();
  for (let entry = started.shift(); entry !== undefined; entry = started.shift()) {
    // Filled again before the entry is handed out: the items after it are under way
    // while the consumer works on this one.
    refill();
    yield entry;
  }
}
