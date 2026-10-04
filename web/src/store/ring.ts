// A bounded list outside the reactive graph: appending is O(1) whatever its
// length, the oldest entry goes when it is full. Lists read it through one
// version signal and compute only the window they show (§perf).

export class Ring<T> {
  private buf: (T | undefined)[];
  private head = 0;
  private len = 0;

  constructor(readonly capacity: number) {
    this.buf = new Array(capacity);
  }

  get length(): number {
    return this.len;
  }

  push(item: T): void {
    this.buf[(this.head + this.len) % this.capacity] = item;
    if (this.len < this.capacity) this.len++;
    else this.head = (this.head + 1) % this.capacity;
  }

  /** Replace the contents with the newest `capacity` of `items` (oldest first). */
  reset(items: readonly T[]): void {
    this.buf = new Array(this.capacity);
    this.head = 0;
    this.len = 0;
    for (const item of items.slice(-this.capacity)) this.push(item);
  }

  /** Put older entries in front (oldest first), as far as there is room. */
  prepend(items: readonly T[]): void {
    const all = [...items, ...this.toArray()];
    this.reset(all.slice(-this.capacity));
  }

  at(i: number): T | undefined {
    if (i < 0) i += this.len;
    if (i < 0 || i >= this.len) return undefined;
    return this.buf[(this.head + i) % this.capacity];
  }

  last(): T | undefined {
    return this.at(this.len - 1);
  }

  slice(from: number, to: number = this.len): T[] {
    from = Math.max(0, from);
    to = Math.min(this.len, to);
    const out: T[] = [];
    for (let i = from; i < to; i++) out.push(this.buf[(this.head + i) % this.capacity] as T);
    return out;
  }

  toArray(): T[] {
    return this.slice(0, this.len);
  }

  /** The newest `n` entries, newest first, without copying the rest. */
  *newest(n: number = this.len): Generator<T> {
    for (let i = this.len - 1; i >= Math.max(0, this.len - n); i--) yield this.buf[(this.head + i) % this.capacity] as T;
  }
}
