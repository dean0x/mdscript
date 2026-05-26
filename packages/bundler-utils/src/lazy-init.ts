/**
 * Single-init lazy value holder with dedup and retry semantics.
 *
 * - Concurrent `get()` calls share the in-flight promise — factory is invoked once.
 * - On factory rejection the pending promise is cleared so the next `get()` retries.
 * - Uses a `resolved` boolean flag so `T = void` and `T = null` work correctly.
 * - A generation counter prevents a stale in-flight factory resolution from
 *   overwriting state that was cleared by `reset()` (TOCTOU safety).
 * - After resolution, `pending` is replaced with a pre-resolved `Promise.resolve(result)`
 *   so subsequent `get()` calls return the same object without allocating a new promise.
 */
export class LazyInit<T> {
  private resolved = false;
  private pending: Promise<T> | null = null;
  private generation = 0;

  constructor(private readonly factory: () => Promise<T>) {}

  /**
   * Returns the cached value, or invokes the factory if not yet resolved.
   * Concurrent calls share the same in-flight promise — factory is invoked once.
   * If the factory rejects, the next call will retry.
   */
  get(): Promise<T> {
    if (this.resolved) return this.pending as Promise<T>;
    if (this.pending === null) {
      const gen = ++this.generation;
      this.pending = this.factory().then(
        (result) => {
          if (this.generation === gen) {
            this.resolved = true;
            this.pending = Promise.resolve(result);
          }
          return result;
        },
        (err: unknown) => {
          if (this.generation === gen) {
            this.pending = null;
          }
          throw err;
        },
      );
    }
    return this.pending;
  }

  /**
   * Clears the cached value and pending promise. The next get() call will
   * re-invoke the factory. Safe to call while a factory is in-flight — the
   * stale result will be discarded via generation counter.
   */
  reset(): void {
    this.generation++;
    this.resolved = false;
    this.pending = null;
  }
}
